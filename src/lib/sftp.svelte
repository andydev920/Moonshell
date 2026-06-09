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
  // SFTP file browser drawer. Reuses the tab's russh connection via sessionId
  // (backend opens a temp channel + request_subsystem("sftp")).
  // Stateless commands: cwd held in frontend, each op passes the full remote path.
  // Bytes bypass JS: upload/download pass only path strings; Rust streams the file locally.
  import { invoke } from "@tauri-apps/api/core";
  import { open as dialogOpen, save as dialogSave } from "@tauri-apps/plugin-dialog";
  import { t as tr } from "$lib/i18n.svelte";

  type Entry = { name: string; isDir: boolean; size: number; mtime: number | null };

  let {
    sessionId,
    title,
    onClose,
    width = 380,
  }: { sessionId: string; title: string; onClose: () => void; width?: number } = $props();

  // cwd = current remote dir. Initial "." resolves server-side to the login (home) dir.
  let cwd = $state(".");
  let pathInput = $state("."); // editable path box, Enter to navigate
  let entries = $state<Entry[]>([]);
  let selected = $state<Entry | null>(null);
  let loading = $state(false);
  let err = $state("");
  let busy = $state(""); // in-progress transfer/op message

  // POSIX join: name alone when cwd === "." (relative to home), else cwd + "/" + name.
  function childPath(name: string): string {
    return cwd === "." ? name : `${cwd}/${name}`;
  }

  // Normalize an input path: strip trailing slashes, empty string becomes ".".
  function normPath(p: string): string {
    const t = p.trim();
    if (t === "" ) return ".";
    if (t === "/") return "/";
    return t.replace(/\/+$/, "");
  }

  async function load(path: string) {
    loading = true;
    err = "";
    try {
      const list = await invoke<Entry[]>("sftp_list", { sessionId, path });
      entries = list;
      cwd = path;
      pathInput = path;
      selected = null;
    } catch (e) {
      err = String(e);
    } finally {
      loading = false;
    }
  }

  function refresh() {
    load(cwd);
  }

  function enterDir(name: string) {
    load(childPath(name));
  }

  function up() {
    if (cwd === "." || cwd === "/") return;
    // Absolute: /a/b -> /a -> / (root); relative: a/b -> a -> . (home)
    const abs = cwd.startsWith("/");
    const segs = cwd.split("/").filter((s) => s.length > 0);
    segs.pop();
    if (segs.length === 0) {
      load(abs ? "/" : ".");
    } else {
      load((abs ? "/" : "") + segs.join("/"));
    }
  }

  function goHome() {
    load(".");
  }

  function gotoInput() {
    load(normPath(pathInput));
  }

  function onRow(entry: Entry) {
    selected = entry;
  }

  function onRowDbl(entry: Entry) {
    if (entry.isDir) enterDir(entry.name);
  }

  // Context menu. entry null = right-click on empty space (dir-level ops only: upload/mkdir/refresh).
  // Position clamped to viewport edges to avoid overflow.
  let menu = $state<{ x: number; y: number; entry: Entry | null } | null>(null);
  const MENU_W = 180;
  const MENU_H = 230;

  function openMenu(ev: MouseEvent, entry: Entry | null) {
    ev.preventDefault();
    ev.stopPropagation();
    if (entry) selected = entry;
    const x = Math.min(ev.clientX, window.innerWidth - MENU_W - 6);
    const y = Math.min(ev.clientY, window.innerHeight - MENU_H - 6);
    menu = { x: Math.max(6, x), y: Math.max(6, y), entry };
  }

  function closeMenu() {
    menu = null;
  }

  // Close the menu before running, so it doesn't linger over a dialog.
  // closeMenu() nulls menu, so callers must capture entry in the closure, not read menu inside it.
  function runMenu(fn: () => void | Promise<void>) {
    closeMenu();
    fn();
  }

  function onWinKey(e: KeyboardEvent) {
    if (e.key === "Escape" && menu) closeMenu();
    else if (e.key === "Escape" && confirmState) confirmNo();
  }

  async function doUpload() {
    err = "";
    let local: string | null;
    try {
      local = await dialogOpen({ multiple: false, directory: false });
    } catch (e) {
      err = tr("sftp.pickFileFail", { e: String(e) });
      return;
    }
    if (!local || typeof local !== "string") return;
    const base = local.split("/").pop() ?? local.split("\\").pop() ?? "upload";
    // Backend sftp.create overwrites (O_TRUNC), so confirm if a same-named entry exists.
    if (entries.some((e) => e.name === base)) {
      if (!(await askDelete(tr("sftp.confirmOverwrite", { name: base })))) return;
    }
    const remote = childPath(base);
    busy = tr("sftp.uploading", { name: base });
    try {
      await invoke("sftp_upload", { sessionId, localPath: local, remotePath: remote });
      busy = "";
      await load(cwd);
    } catch (e) {
      busy = "";
      err = String(e);
    }
  }

  async function doDownload(target: Entry | null = selected) {
    if (!target || target.isDir) return;
    const entry = target;
    err = "";
    let local: string | null;
    try {
      local = await dialogSave({ defaultPath: entry.name });
    } catch (e) {
      err = tr("sftp.pickSaveFail", { e: String(e) });
      return;
    }
    if (!local) return;
    busy = tr("sftp.downloading", { name: entry.name });
    try {
      await invoke("sftp_download", {
        sessionId,
        remotePath: childPath(entry.name),
        localPath: local,
      });
      busy = "";
    } catch (e) {
      busy = "";
      err = String(e);
    }
  }

  // WKWebView disables window.prompt() (returns null), so use an inline modal.
  // askText returns a Promise that resolves on confirm/cancel.
  let ask = $state<{ label: string; value: string; resolve: (v: string | null) => void } | null>(null);
  let askInput = $state<HTMLInputElement | null>(null);

  function askText(label: string, initial = ""): Promise<string | null> {
    return new Promise((resolve) => {
      ask = { label, value: initial, resolve };
      // Focus and select existing text (eases full replace on rename).
      queueMicrotask(() => askInput?.focus());
      queueMicrotask(() => askInput?.select());
    });
  }

  function askConfirm() {
    const a = ask;
    ask = null;
    a?.resolve(a.value);
  }

  function askCancel() {
    const a = ask;
    ask = null;
    a?.resolve(null);
  }

  // WKWebView also disables window.confirm() (returns false), so delete confirm uses an inline modal too.
  let confirmState = $state<{ message: string; resolve: (ok: boolean) => void } | null>(null);

  function askDelete(message: string): Promise<boolean> {
    return new Promise((resolve) => {
      confirmState = { message, resolve };
    });
  }

  function confirmYes() {
    const c = confirmState;
    confirmState = null;
    c?.resolve(true);
  }

  function confirmNo() {
    const c = confirmState;
    confirmState = null;
    c?.resolve(false);
  }

  async function doMkdir() {
    const name = await askText(tr("sftp.mkdirPrompt"));
    if (!name) return;
    const trimmed = name.trim();
    if (!trimmed) return;
    err = "";
    try {
      await invoke("sftp_mkdir", { sessionId, path: childPath(trimmed) });
      await load(cwd);
    } catch (e) {
      err = String(e);
    }
  }

  async function doRemove(target: Entry | null = selected) {
    if (!target) return;
    const entry = target;
    const kind = entry.isDir ? tr("sftp.dirKind") : tr("sftp.fileKind");
    if (!(await askDelete(tr("sftp.confirmDelete", { kind, name: entry.name })))) return;
    err = "";
    try {
      await invoke("sftp_remove", {
        sessionId,
        path: childPath(entry.name),
        isDir: entry.isDir,
      });
      await load(cwd);
    } catch (e) {
      err = String(e);
    }
  }

  async function doRename(target: Entry | null = selected) {
    if (!target) return;
    const entry = target;
    const next = await askText(tr("sftp.renamePrompt"), entry.name);
    if (!next) return;
    const trimmed = next.trim();
    if (!trimmed || trimmed === entry.name) return;
    err = "";
    try {
      await invoke("sftp_rename", {
        sessionId,
        oldPath: childPath(entry.name),
        newPath: childPath(trimmed),
      });
      await load(cwd);
    } catch (e) {
      err = String(e);
    }
  }

  // Human-readable size.
  function fmtSize(n: number): string {
    if (n < 1024) return `${n} B`;
    const u = ["KB", "MB", "GB", "TB"];
    let v = n / 1024;
    let i = 0;
    while (v >= 1024 && i < u.length - 1) {
      v /= 1024;
      i++;
    }
    return `${v.toFixed(v >= 10 ? 0 : 1)} ${u[i]}`;
  }

  function fmtTime(t: number | null): string {
    if (!t) return "";
    const d = new Date(t * 1000);
    const p = (n: number) => String(n).padStart(2, "0");
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  }

  function onPathKey(e: KeyboardEvent) {
    if (e.key === "Enter") gotoInput();
  }

  // On mount: load the login dir; reload if sessionId changes.
  $effect(() => {
    load(".");
  });
</script>

<svelte:window onkeydown={onWinKey} onclick={closeMenu} onresize={closeMenu} onblur={closeMenu} />

<aside class="drawer" style="width: {width}px">
  <header class="dhead">
    <span class="dtitle" title={title}><span class="dtag">SFTP</span> · {title}</span>
    <button class="close" title={tr("common.close")} onclick={onClose}>×</button>
  </header>

  <div class="nav">
    <button class="nbtn" title={tr("sftp.home")} onclick={goHome} aria-label={tr("sftp.home")}>
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2.5 7 8 2.5 13.5 7M4 6v7h8V6" /></svg>
    </button>
    <button class="nbtn" title={tr("sftp.parent")} onclick={up} disabled={cwd === "." || cwd === "/"} aria-label={tr("sftp.parent")}>
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M8 13V4M4 7.5 8 3.5l4 4" /></svg>
    </button>
    <input
      class="path"
      bind:value={pathInput}
      onkeydown={onPathKey}
      spellcheck="false"
      placeholder={tr("sftp.pathPh")}
    />
    <button class="nbtn" title={tr("sftp.refresh")} onclick={refresh} aria-label={tr("sftp.refresh")}>
      <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M13 7a5 5 0 1 0-.5 3M13 3.5V7H9.5" /></svg>
    </button>
  </div>

  <div class="ops">
    <button class="obtn" onclick={doUpload}>
      <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M8 11V3M5 6 8 3l3 3M3 13h10" /></svg>{tr("sftp.upload")}
    </button>
    <button class="obtn" onclick={doMkdir}>
      <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4.5a1 1 0 0 1 1-1h2.5L7 5h6a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z" /><path d="M8 7.5v3M6.5 9h3" /></svg>{tr("sftp.mkdir")}
    </button>
    {#if selected}
      <span class="opsep"></span>
      {#if !selected.isDir}
        <button class="obtn" onclick={() => doDownload()}>
          <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M8 3v8M5 8l3 3 3-3M3 13h10" /></svg>{tr("sftp.download")}
        </button>
      {/if}
      <button class="obtn" onclick={() => doRename()}>{tr("sftp.rename")}</button>
      <button class="obtn danger" onclick={() => doRemove()}>{tr("common.delete")}</button>
    {/if}
  </div>

  {#if busy}<div class="banner busy">{busy}</div>{/if}
  {#if err}<div class="banner err">{err}</div>{/if}

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="list" oncontextmenu={(ev) => openMenu(ev, null)} onscroll={closeMenu}>
    {#if loading}
      <div class="placeholder">{tr("sftp.loading")}</div>
    {:else if entries.length === 0}
      <div class="placeholder">{tr("sftp.empty")}</div>
    {:else}
      {#each entries as e (e.name)}
        <div
          class="row"
          class:sel={selected?.name === e.name}
          role="button"
          tabindex="0"
          onclick={() => onRow(e)}
          ondblclick={() => onRowDbl(e)}
          oncontextmenu={(ev) => openMenu(ev, e)}
          onkeydown={(ev) => {
            if (ev.key === "Enter") e.isDir ? enterDir(e.name) : onRow(e);
          }}
        >
          <span class="ficon" class:dir={e.isDir}>
            {#if e.isDir}
              <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"><path d="M2 4.5a1 1 0 0 1 1-1h2.8l1.3 1.4H13a1 1 0 0 1 1 1v5.6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z" /></svg>
            {:else}
              <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round"><path d="M4 2.5h4.5L12 6v7.5H4Z" /><path d="M8.4 2.5V6H12" /></svg>
            {/if}
          </span>
          <span class="fname" title={e.name}>{e.name}</span>
          <span class="fsize">{e.isDir ? "" : fmtSize(e.size)}</span>
          <span class="fmtime">{fmtTime(e.mtime)}</span>
        </div>
      {/each}
    {/if}
  </div>

  {#if menu}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="ctxmenu"
      role="menu"
      tabindex="-1"
      style="left: {menu.x}px; top: {menu.y}px"
      oncontextmenu={(ev) => ev.preventDefault()}
    >
      {#if menu.entry}
        {#if menu.entry.isDir}
          <button class="mi" role="menuitem" onclick={() => { const e = menu!.entry!; runMenu(() => enterDir(e.name)); }}>
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4.5a1 1 0 0 1 1-1h2.8l1.3 1.4H13a1 1 0 0 1 1 1v5.6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z" /></svg>{tr("sftp.open")}
          </button>
        {:else}
          <button class="mi" role="menuitem" onclick={() => { const e = menu!.entry!; runMenu(() => doDownload(e)); }}>
            <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M8 3v8M5 8l3 3 3-3M3 13h10" /></svg>{tr("sftp.download")}
          </button>
        {/if}
        <button class="mi" role="menuitem" onclick={() => { const e = menu!.entry!; runMenu(() => doRename(e)); }}>
          <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M9 3.5 12.5 7 6 13.5H2.5V10L9 3.5Z" /></svg>{tr("sftp.rename")}
        </button>
        <button class="mi danger" role="menuitem" onclick={() => { const e = menu!.entry!; runMenu(() => doRemove(e)); }}>
          <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M3 4.5h10M6.5 4V3h3v1M5 4.5l.6 8h4.8l.6-8" /></svg>{tr("common.delete")}
        </button>
        <span class="msep"></span>
      {/if}
      <button class="mi" role="menuitem" onclick={() => runMenu(doUpload)}>
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M8 11V3M5 6 8 3l3 3M3 13h10" /></svg>{tr("sftp.upload")}
      </button>
      <button class="mi" role="menuitem" onclick={() => runMenu(doMkdir)}>
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M2 4.5a1 1 0 0 1 1-1h2.5L7 5h6a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z" /><path d="M8 7.5v3M6.5 9h3" /></svg>{tr("sftp.mkdir")}
      </button>
      <button class="mi" role="menuitem" onclick={() => runMenu(refresh)}>
        <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"><path d="M13 7a5 5 0 1 0-.5 3M13 3.5V7H9.5" /></svg>{tr("sftp.refresh")}
      </button>
    </div>
  {/if}

  {#if ask}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="modal-mask" onclick={askCancel}>
      <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
      <div class="modal" onclick={(ev) => ev.stopPropagation()}>
        <div class="modal-label">{ask.label}</div>
        <input
          class="modal-input"
          bind:this={askInput}
          bind:value={ask.value}
          spellcheck="false"
          autocomplete="off"
          onkeydown={(ev) => {
            if (ev.key === "Enter") { ev.preventDefault(); askConfirm(); }
            else if (ev.key === "Escape") { ev.preventDefault(); askCancel(); }
          }}
        />
        <div class="modal-actions">
          <button class="modal-btn" onclick={askCancel}>{tr("common.cancel")}</button>
          <button class="modal-btn primary" onclick={askConfirm}>{tr("common.ok")}</button>
        </div>
      </div>
    </div>
  {/if}

  {#if confirmState}
    <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
    <div class="modal-mask" onclick={confirmNo}>
      <!-- svelte-ignore a11y_no_static_element_interactions, a11y_click_events_have_key_events -->
      <div class="modal" onclick={(ev) => ev.stopPropagation()}>
        <div class="modal-label">{confirmState.message}</div>
        <div class="modal-actions">
          <button class="modal-btn" onclick={confirmNo}>{tr("common.cancel")}</button>
          <button class="modal-btn danger" onclick={confirmYes}>{tr("common.delete")}</button>
        </div>
      </div>
    </div>
  {/if}
</aside>

<style>
  .drawer {
    width: 380px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    background: var(--surface-1);
    border-left: 1px solid var(--line);
    box-shadow: -2px 0 8px rgba(0, 0, 0, 0.25);
    height: 100%;
    min-height: 0;
    position: relative;
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
  }
  .dtitle { font-variant-numeric: tabular-nums; }
  .dtag { color: var(--blue); font-weight: 700; letter-spacing: 0.3px; }
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
  .close:hover { color: var(--red); background: var(--surface-3); }

  .nav {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
  }
  .nbtn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    color: var(--text-dim);
    border-radius: var(--r-sm);
    min-width: 30px;
    height: 30px;
    cursor: pointer;
    padding: 0 7px;
    transition: border-color 0.12s, color 0.12s, background 0.12s;
  }
  .nbtn:hover:not(:disabled) { border-color: var(--blue); color: var(--blue); background: var(--surface-3); }
  .nbtn:active:not(:disabled) { transform: translateY(0.5px); }
  .nbtn:disabled { opacity: 0.35; cursor: default; }
  .path {
    flex: 1;
    min-width: 0;
    background: var(--bg);
    border: 1px solid var(--line);
    color: var(--text);
    border-radius: var(--r-sm);
    padding: 6px 9px;
    font-size: 12px;
    font-family: Menlo, "SF Mono", monospace;
    outline: none;
    transition: border-color 0.12s, box-shadow 0.12s;
  }
  .path:focus { border-color: var(--blue); box-shadow: var(--focus-ring); }

  .ops {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--line);
    min-height: 45px;
  }
  .opsep {
    width: 1px;
    align-self: stretch;
    margin: 2px 2px;
    background: var(--line-strong);
  }
  .obtn {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    color: var(--text);
    border-radius: var(--r-sm);
    padding: 5px 10px;
    height: 28px;
    font-size: 12px;
    cursor: pointer;
    transition: border-color 0.12s, color 0.12s, background 0.12s;
  }
  .obtn:hover:not(:disabled) { border-color: var(--blue); color: var(--blue); background: var(--surface-3); }
  .obtn.danger { color: var(--text-dim); }
  .obtn.danger:hover:not(:disabled) { border-color: var(--red); color: var(--red); background: var(--red-soft); }
  .obtn:active:not(:disabled) { transform: translateY(0.5px); }
  .obtn:disabled { opacity: 0.4; cursor: default; }

  .banner {
    padding: 7px 14px;
    font-size: 12px;
    border-bottom: 1px solid var(--line);
  }
  .banner.busy { color: var(--yellow); }
  .banner.err { color: var(--red); word-break: break-all; background: var(--red-soft); }

  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 4px 0;
  }
  .placeholder {
    color: var(--text-mute);
    font-size: 13px;
    text-align: center;
    padding: 28px 0;
  }
  .row {
    position: relative;
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 6px 14px;
    cursor: pointer;
    border: none;
    background: none;
    width: 100%;
    text-align: left;
    transition: background 0.1s;
  }
  .row:hover { background: var(--surface-2); }
  .row.sel { background: var(--blue-soft); }
  .row.sel::before {
    content: "";
    position: absolute;
    left: 0;
    top: 4px;
    bottom: 4px;
    width: 2px;
    background: var(--blue);
  }
  .ficon {
    flex-shrink: 0;
    display: inline-flex;
    color: var(--text-mute);
  }
  .ficon.dir { color: var(--blue); }
  .fname {
    flex: 1;
    min-width: 0;
    color: var(--text);
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .fsize {
    flex-shrink: 0;
    color: var(--text-mute);
    font-size: 12px;
    width: 64px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .fmtime {
    flex-shrink: 0;
    color: var(--text-mute);
    font-size: 11px;
    width: 104px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }

  .ctxmenu {
    position: fixed;
    z-index: 50;
    min-width: 160px;
    padding: 4px;
    background: var(--surface-1);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.35);
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .mi {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    background: none;
    border: none;
    color: var(--text);
    font-size: 13px;
    text-align: left;
    padding: 6px 9px;
    border-radius: 4px;
    cursor: pointer;
    transition: background 0.1s, color 0.1s;
  }
  .mi > svg { color: var(--text-mute); flex-shrink: 0; }
  .mi:hover { background: var(--blue-soft); color: var(--blue); }
  .mi:hover > svg { color: var(--blue); }
  .mi.danger:hover { background: var(--red-soft); color: var(--red); }
  .mi.danger:hover > svg { color: var(--red); }
  .msep {
    height: 1px;
    margin: 3px 4px;
    background: var(--line);
  }

  /* Inline input modal (replaces WKWebView-disabled window.prompt). */
  .modal-mask {
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 50;
  }
  .modal {
    width: calc(100% - 48px);
    max-width: 320px;
    background: var(--surface-1);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-md, 8px);
    box-shadow: 0 8px 28px rgba(0, 0, 0, 0.45);
    padding: 16px;
  }
  .modal-label {
    color: var(--text-dim);
    font-size: 13px;
    font-weight: 600;
    margin-bottom: 10px;
  }
  .modal-input {
    width: 100%;
    box-sizing: border-box;
    background: var(--bg);
    border: 1px solid var(--line);
    color: var(--text);
    border-radius: var(--r-sm);
    padding: 7px 9px;
    font-size: 13px;
    font-family: Menlo, "SF Mono", monospace;
    outline: none;
    transition: border-color 0.12s, box-shadow 0.12s;
  }
  .modal-input:focus { border-color: var(--blue); box-shadow: var(--focus-ring); }
  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 14px;
  }
  .modal-btn {
    background: var(--surface-2);
    border: 1px solid var(--line-strong);
    color: var(--text-dim);
    border-radius: var(--r-sm);
    padding: 6px 14px;
    font-size: 12px;
    cursor: pointer;
    transition: border-color 0.12s, color 0.12s, background 0.12s;
  }
  .modal-btn:hover { border-color: var(--blue); color: var(--blue); background: var(--surface-3); }
  .modal-btn.primary {
    background: var(--blue);
    border-color: var(--blue);
    color: #fff;
  }
  .modal-btn.primary:hover { filter: brightness(1.08); color: #fff; }
  .modal-btn.danger {
    background: var(--red);
    border-color: var(--red);
    color: #fff;
  }
  .modal-btn.danger:hover { filter: brightness(1.08); color: #fff; }
</style>
