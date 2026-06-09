<!--
  Copyright 2026 Moonya

  Licensed under the Apache License, Version 2.0 (the "License");
  you may not use this file except in compliance with the License.
  You may obtain a copy of the License at

      http://www.apache.org/licenses/LICENSE-2.0

  Unless required by applicable law or agreed to in writing, software
  distributed under the License is distributed on an "AS IS" BASIS,
  WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
  See the License for the specific language governing permissions and
  limitations under the License.
-->
<script lang="ts">
  // Server monitor drawer. Reuses the tab's russh connection: ssh_exec opens a temp exec
  // channel by sessionId and runs one aggregate script. Front-end polls every ~2s while open;
  // parsing and process search are front-end only.
  import { invoke } from "@tauri-apps/api/core";
  import { t as tr } from "$lib/i18n.svelte";

  let {
    sessionId,
    title,
    onClose,
    width = 380,
  }: { sessionId: string; title: string; onClose: () => void; width?: number } = $props();

  type Metrics = {
    cpu: number | null;
    mem: number | null;
    disk: number | null;
    netup: string | null;
    netdown: string | null;
    os: string | null;
    kernel: string | null;
  };
  type Proc = { pid: string; user: string; mem: string; cpu: string; cmd: string };

  let metrics = $state<Metrics>({
    cpu: null, mem: null, disk: null, netup: null, netdown: null, os: null, kernel: null,
  });
  let procs = $state<Proc[]>([]);
  let q = $state(""); // process search term (front-end filter)
  let loading = $state(true); // true only on first frame
  let err = $state(""); // sample failure -> top red bar; keeps retrying

  const POLL_MS = 2000;

  // Sample command: one round-trip for all metrics + process list, with sleep 1 for CPU/net deltas.
  // Uses /proc + df + ps + awk + uname; missing fields emit empty strings, shown as "-".
  // Output contract: line 1 is key=value (| separated, 7 keys), @@PROC@@ on its own line as a
  // sentinel, then 5 fields per line (first 4 spaceless, field 5+ is the command).
  const SAMPLE_CMD =
    `sh -c 'kver=$(uname -r 2>/dev/null);` +
    `os=$(. /etc/os-release 2>/dev/null; printf "%s" "$PRETTY_NAME");` +
    `cpu0=$(awk "NR==1{tot=0;for(i=2;i<=NF;i++)tot+=\\$i; print tot\\" \\"\\$5}" /proc/stat);` +
    `net0=$(awk -F"[: ]+" "/:/{if(\\$1!~/lo/){r+=\\$3;t+=\\$11}}END{print r\\" \\"t}" /proc/net/dev);` +
    `sleep 1;` +
    `cpu1=$(awk "NR==1{tot=0;for(i=2;i<=NF;i++)tot+=\\$i; print tot\\" \\"\\$5}" /proc/stat);` +
    `net1=$(awk -F"[: ]+" "/:/{if(\\$1!~/lo/){r+=\\$3;t+=\\$11}}END{print r\\" \\"t}" /proc/net/dev);` +
    `cpu=$(echo "$cpu0 $cpu1" | awk "{dt=\\$3-\\$1; di=\\$4-\\$2; if(dt<=0){print 0}else{printf \\"%d\\", (dt-di)*100/dt}}");` +
    `mem=$(awk "/MemTotal/{t=\\$2}/MemAvailable/{a=\\$2}END{if(t>0)printf \\"%d\\",(t-a)*100/t; else print \\"\\"}" /proc/meminfo);` +
    `disk=$(df -P / 2>/dev/null | awk "NR==2{gsub(/%/,\\"\\",\\$5); print \\$5}");` +
    `netdown=$(echo "$net0 $net1" | awk "{printf \\"%.1f\\",(\\$3-\\$1)/1024}");` +
    `netup=$(echo "$net0 $net1" | awk "{printf \\"%.1f\\",(\\$4-\\$2)/1024}");` +
    `printf "cpu=%s|mem=%s|disk=%s|netup=%s|netdown=%s|os=%s|kernel=%s\\n" "$cpu" "$mem" "$disk" "$netup" "$netdown" "$os" "$kver";` +
    `printf "@@PROC@@\\n";` +
    `ps -eo pid,user,pmem,pcpu,comm --sort=-pcpu 2>/dev/null | awk "NR>1 && NR<=21{print \\$1\\" \\"\\$2\\" \\"\\$3\\" \\"\\$4\\" \\"\\$5}"'`;

  // Parse stdout: KV section, @@PROC@@ separator, then process rows.
  function parse(raw: string) {
    const idx = raw.indexOf("@@PROC@@");
    const head = idx >= 0 ? raw.slice(0, idx) : raw;
    const body = idx >= 0 ? raw.slice(idx + "@@PROC@@".length) : "";

    const kv: Record<string, string> = {};
    // KV section may have leading blank lines (login-script noise); take the last non-empty line.
    const lines = head.trim().split("\n");
    const line = lines.length ? lines[lines.length - 1] : "";
    for (const pair of line.split("|")) {
      const eq = pair.indexOf("=");
      if (eq > 0) kv[pair.slice(0, eq)] = pair.slice(eq + 1);
    }
    const num = (s?: string) => (s && s.trim() !== "" ? Number(s) : null);
    const str = (s?: string) => (s && s.trim() !== "" ? s.trim() : null);
    metrics = {
      cpu: num(kv.cpu), mem: num(kv.mem), disk: num(kv.disk),
      netup: str(kv.netup), netdown: str(kv.netdown),
      os: str(kv.os), kernel: str(kv.kernel),
    };

    const rows: Proc[] = [];
    for (const ln of body.split("\n")) {
      const s = ln.trim();
      if (!s) continue;
      const parts = s.split(/\s+/);
      if (parts.length < 5) continue;
      // First 4 fields fixed; join the rest as the command (may contain spaces).
      rows.push({
        pid: parts[0], user: parts[1], mem: parts[2], cpu: parts[3],
        cmd: parts.slice(4).join(" "),
      });
    }
    procs = rows;
  }

  async function sample() {
    try {
      const raw = await invoke<string>("ssh_exec", { sessionId, command: SAMPLE_CMD });
      parse(raw);
      err = "";
    } catch (e) {
      err = String(e); // top red bar; keep existing data and retry
    } finally {
      loading = false;
    }
  }

  // Polling: self-scheduling setTimeout chain, not setInterval, so frames stay serial with no
  // overlap (setInterval would fire concurrent ssh_exec channels on slow links).
  // On sessionId change or unmount, stopped + clearTimeout prevent scheduling another frame.
  $effect(() => {
    const sid = sessionId; // dependency: rebuild when sid changes
    void sid;
    loading = true;
    procs = [];
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const loop = async () => {
      await sample();
      if (stopped) return; // unmounted/session changed during sample: stop scheduling
      timer = setTimeout(loop, POLL_MS);
    };
    loop(); // sample immediately, don't wait the first 2s
    return () => {
      stopped = true;
      if (timer) clearTimeout(timer);
    };
  });

  // Process search: matches command / user / PID (front-end filter).
  const shown = $derived(
    q.trim() === ""
      ? procs
      : procs.filter((p) => {
          const s = q.toLowerCase();
          return (
            p.cmd.toLowerCase().includes(s) ||
            p.user.toLowerCase().includes(s) ||
            p.pid.includes(s)
          );
        }),
  );

  // Progress-bar thresholds: <60 ok / 60-85 warn / >85 danger.
  function level(v: number | null): "ok" | "warn" | "danger" {
    if (v === null) return "ok";
    if (v > 85) return "danger";
    if (v >= 60) return "warn";
    return "ok";
  }
  const dash = (v: unknown) => (v === null || v === undefined || v === "" ? "-" : String(v));
</script>

<aside class="drawer" style="width: {width}px">
  <header class="dhead">
    <span class="dtitle" title={title}><span class="dtag">{tr("monitor.title")}</span> · {title}</span>
    <button class="close" title={tr("common.close")} onclick={onClose}>×</button>
  </header>

  {#if err}<div class="errbar">{err}</div>{/if}

  <div class="mbody">
    <!-- Metrics: three progress bars + text cards -->
    <div class="grid">
      {#each [["cpu", metrics.cpu], ["mem", metrics.mem], ["disk", metrics.disk]] as [k, v] (k)}
        <div class="metric">
          <div class="mlabel">
            <span>{tr(`monitor.${k}`)}</span>
            <span class="mval">{v === null ? "-" : v + "%"}</span>
          </div>
          <div class="bar"><div class="fill {level(v as number | null)}" style="width: {v ?? 0}%"></div></div>
        </div>
      {/each}
      <div class="kv"><span>{tr("monitor.netUp")}</span><b>{dash(metrics.netup)} KB/s</b></div>
      <div class="kv"><span>{tr("monitor.netDown")}</span><b>{dash(metrics.netdown)} KB/s</b></div>
      <div class="kv"><span>{tr("monitor.os")}</span><b title={metrics.os ?? ""}>{dash(metrics.os)}</b></div>
      <div class="kv"><span>{tr("monitor.kernel")}</span><b title={metrics.kernel ?? ""}>{dash(metrics.kernel)}</b></div>
    </div>

    <!-- Processes: search + table -->
    <input class="search" placeholder={tr("monitor.procSearch")} bind:value={q} spellcheck="false" />
    {#if loading}
      <div class="empty">{tr("monitor.loading")}</div>
    {:else if shown.length === 0}
      <div class="empty">{tr("monitor.empty")}</div>
    {:else}
      <div class="ptwrap">
        <table class="ptable">
          <thead>
            <tr>
              <th>{tr("monitor.pid")}</th>
              <th>{tr("monitor.user")}</th>
              <th class="num">{tr("monitor.memPct")}</th>
              <th class="num">{tr("monitor.cpuPct")}</th>
              <th>{tr("monitor.cmd")}</th>
            </tr>
          </thead>
          <tbody>
            {#each shown as p (p.pid)}
              <tr>
                <td>{p.pid}</td>
                <td>{p.user}</td>
                <td class="num">{p.mem}</td>
                <td class="num">{p.cpu}</td>
                <td class="cmd" title={p.cmd}>{p.cmd}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
</aside>

<style>
  /* .drawer/.dhead/.dtitle/.dtag/.close mirror sftp.svelte; kept here since Svelte styles are scoped. */
  .drawer {
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: var(--surface-1);
    border-left: 1px solid var(--line);
    box-shadow: -2px 0 8px rgba(0, 0, 0, 0.25);
    height: 100%;
    min-height: 0;
  }
  .dhead {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 14px;
    border-bottom: 1px solid var(--line);
  }
  .dtitle {
    color: var(--text-dim);
    font-size: 13px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    font-variant-numeric: tabular-nums;
  }
  .dtag {
    color: var(--blue);
    font-weight: 700;
    letter-spacing: 0.3px;
  }
  .close {
    background: none;
    border: none;
    color: var(--text-mute);
    cursor: pointer;
    font-size: 18px;
    line-height: 1;
    padding: 0 4px;
    border-radius: 4px;
    transition: color 0.12s, background 0.12s;
  }
  .close:hover {
    color: var(--red);
    background: var(--surface-3);
  }

  .errbar {
    background: var(--red-soft);
    color: var(--red);
    padding: 7px 14px;
    font-size: 12px;
    border-bottom: 1px solid var(--line);
    word-break: break-all;
  }

  .mbody {
    flex: 1;
    min-height: 0;
    padding: 12px;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }
  .metric {
    grid-column: 1 / -1;
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 8px 10px;
  }
  .mlabel {
    display: flex;
    justify-content: space-between;
    font-size: 12px;
    color: var(--text-dim);
    margin-bottom: 6px;
  }
  .mval {
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }
  .bar {
    height: 6px;
    background: var(--surface-3);
    border-radius: 4px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    transition: width 0.3s ease, background 0.2s;
  }
  /* Threshold colors use semantic vars, defined per theme. */
  .fill.ok {
    background: var(--green);
  }
  .fill.warn {
    background: var(--yellow);
  }
  .fill.danger {
    background: var(--red);
  }
  .kv {
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .kv span {
    font-size: 11px;
    color: var(--text-dim);
  }
  .kv b {
    font-size: 13px;
    color: var(--text);
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .search {
    width: 100%;
    box-sizing: border-box;
    background: var(--bg);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 6px 9px;
    color: var(--text);
    font-size: 12px;
    outline: none;
    transition: border-color 0.12s, box-shadow 0.12s;
  }
  .search:focus {
    border-color: var(--blue);
    box-shadow: var(--focus-ring);
  }
  .search::placeholder {
    color: var(--text-mute);
  }

  .ptwrap {
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    overflow: auto;
  }
  .ptable {
    width: 100%;
    border-collapse: collapse;
    font-size: 12px;
    table-layout: fixed;
  }
  .ptable th {
    text-align: left;
    color: var(--text-dim);
    font-weight: 500;
    padding: 5px 8px;
    border-bottom: 1px solid var(--line);
    position: sticky;
    top: 0;
    background: var(--surface-2);
    white-space: nowrap;
  }
  .ptable th.num,
  .ptable td.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .ptable td {
    padding: 5px 8px;
    color: var(--text);
    border-bottom: 1px solid var(--line);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .ptable tbody tr:last-child td {
    border-bottom: none;
  }
  .ptable tbody tr:hover td {
    background: var(--surface-2);
  }
  /* Command column fills remaining width, truncates with ellipsis; full text in hover title. */
  .ptable td.cmd {
    width: 100%;
    color: var(--text-dim);
  }
  .empty {
    color: var(--text-mute);
    font-size: 13px;
    padding: 24px 0;
    text-align: center;
  }
</style>
