<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { invoke, Channel } from "@tauri-apps/api/core";
  import { listen, type UnlistenFn } from "@tauri-apps/api/event";
  import { Terminal } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";
  import { hosts, type Host, type AuthKind } from "$lib/hosts.svelte";
  import Sftp from "$lib/sftp.svelte";
  import Monitor from "$lib/monitor.svelte";
  // i18n: alias t() to tr, avoiding the tab variable t used throughout the template.
  import { t as tr, i18n, LOCALES } from "$lib/i18n.svelte";
  import { settings, ACCENTS, TERM_FONT_MIN, TERM_FONT_MAX } from "$lib/settings.svelte";
  import type { ThemeMode } from "$lib/settings.svelte";

  // Theme mode options, in display order.
  const MODES: { key: ThemeMode; label: string }[] = [
    { key: "light", label: "settings.modeLight" },
    { key: "dark", label: "settings.modeDark" },
    { key: "auto", label: "settings.modeAuto" },
  ];

  // One tab = one SSH session. States:
  //   form        awaiting password to connect (or returned here after disconnect)
  //   connecting  connecting
  //   open        connected, terminal usable
  //   closed      connection closed (peer or local)
  type Status = "form" | "connecting" | "open" | "closed";
  type Tab = {
    id: string; // = sessionId, backend routes by it
    kind: "ssh" | "host-form" | "settings"; // ssh session / host add-edit form / settings tab
    hostId?: string; // saved host id (Keychain account); empty for manual/unsaved connections
    label: string;
    host: string;
    port: number;
    user: string;
    // Password mode: holds typed password during form only; key mode: holds passphrase. Cleared once connected.
    password: string;
    auth: AuthKind; // auth method (carried from Host)
    keyPath: string; // private key path (when auth==='key')
    savePassword: boolean; // whether this host has a password in Keychain
    status: Status;
    msg: string;
  };

  // xterm instances are non-reactive; managed by id in a plain Map.
  type Bundle = {
    term: Terminal;
    fit: FitAddon;
    unlisteners: UnlistenFn[];
    lastCols: number;
    lastRows: number;
  };
  const terms = new Map<string, Bundle>();

  let tabs = $state<Tab[]>([]);
  let activeId = $state<string | null>(null);
  const active = $derived(tabs.find((t) => t.id === activeId) ?? null);

  // host -> highest-priority session status (open>connecting), for the sidebar connection dot.
  const hostStatus = $derived.by(() => {
    const m = new Map<string, Status>();
    for (const t of tabs) {
      if (!t.hostId) continue;
      if (t.status === "open") m.set(t.hostId, "open");
      else if (t.status === "connecting" && m.get(t.hostId) !== "open")
        m.set(t.hostId, "connecting");
    }
    return m;
  });

  // Host search filter (frontend-only).
  let hostFilter = $state("");
  const filteredHosts = $derived.by(() => {
    const q = hostFilter.trim().toLowerCase();
    if (!q) return hosts.list;
    return hosts.list.filter(
      (h) =>
        h.label.toLowerCase().includes(q) ||
        h.host.toLowerCase().includes(q) ||
        h.user.toLowerCase().includes(q),
    );
  });

  // Interactive host-key trust prompts (emitted by the backend on first connect to an unknown host).
  // Queued so concurrent connects each get answered; the modal shows queue[0], the answer dequeues it.
  type HostKeyPrompt = {
    requestId: number;
    host: string;
    port: number;
    algorithm: string;
    fingerprint: string;
  };
  let hostKeyQueue = $state<HostKeyPrompt[]>([]);
  const hostKeyPrompt = $derived(hostKeyQueue[0] ?? null);

  // Answer the front-most host-key prompt: tell the backend, then dequeue.
  async function decideHostKey(trust: boolean) {
    const p = hostKeyQueue[0];
    if (!p) return;
    hostKeyQueue = hostKeyQueue.slice(1);
    try {
      await invoke("ssh_hostkey_decision", { requestId: p.requestId, trust });
    } catch {
      // Backend prompt already gone (handshake aborted) — nothing to do.
    }
  }

  // sessionId (= tab.id) of the open SFTP panel; null if none.
  let sftpOpenId = $state<string | null>(null);
  // Panel title shows the tab's user@host
  const sftpTitle = $derived(
    (() => {
      const t = tabs.find((x) => x.id === sftpOpenId);
      return t ? `${t.user}@${t.host}` : "";
    })(),
  );

  // Monitor panel, parallel to SFTP. sessionId (= tab.id) of the open monitor; null if none.
  let monitorOpenId = $state<string | null>(null);
  const monitorTitle = $derived(
    (() => {
      const t = tabs.find((x) => x.id === monitorOpenId);
      return t ? `${t.user}@${t.host}` : "";
    })(),
  );

  // Sidebar selection (highlight only) and add/edit host form
  let selectedId = $state<string | null>(null);
  let formEditId = $state<string | null>(null);
  let fLabel = $state("");
  let fHost = $state("");
  let fPort = $state(22);
  let fUser = $state("root");
  let fPass = $state("");
  let fAuth = $state<AuthKind>("password"); // auth method
  let fKeyPath = $state(""); // private key path (fAuth==='key')
  let fSavePass = $state(false); // save password to Keychain

  let stackEl: HTMLDivElement;
  let passEl = $state<HTMLInputElement>();
  let resizeTimer: ReturnType<typeof setTimeout> | undefined;

  // Sidebar / SFTP panel widths, draggable, persisted to localStorage.
  const SIDEBAR_MIN = 160, SIDEBAR_MAX = 360;
  const SFTP_MIN = 280, SFTP_MAX = 600;
  const MONITOR_MIN = 320, MONITOR_MAX = 640;
  function loadW(key: string, def: number) {
    const v = Number(localStorage.getItem(key));
    return Number.isFinite(v) && v > 0 ? v : def;
  }
  let sidebarW = $state(loadW("moonshell.sidebarW", 245));
  let sftpW = $state(loadW("moonshell.sftpW", 380));
  let monitorW = $state(loadW("moonshell.monitorW", 380));

  // Drag separator: which picks the panel. sidebar widens rightward, sftp/monitor leftward.
  function startDrag(e: PointerEvent, which: "sidebar" | "sftp" | "monitor") {
    e.preventDefault();
    const startX = e.clientX;
    const startW = which === "sidebar" ? sidebarW : which === "sftp" ? sftpW : monitorW;
    const dir = which === "sidebar" ? 1 : -1;
    const min = which === "sidebar" ? SIDEBAR_MIN : which === "sftp" ? SFTP_MIN : MONITOR_MIN;
    const max = which === "sidebar" ? SIDEBAR_MAX : which === "sftp" ? SFTP_MAX : MONITOR_MAX;
    const move = (ev: PointerEvent) => {
      const w = Math.max(min, Math.min(max, startW + dir * (ev.clientX - startX)));
      if (which === "sidebar") sidebarW = w;
      else if (which === "sftp") sftpW = w;
      else monitorW = w;
      if (activeId) fitOne(activeId); // live refit; fitOne dedups on cols/rows
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      const key =
        which === "sidebar"
          ? "moonshell.sidebarW"
          : which === "sftp"
            ? "moonshell.sftpW"
            : "moonshell.monitorW";
      const val = which === "sidebar" ? sidebarW : which === "sftp" ? sftpW : monitorW;
      localStorage.setItem(key, String(val));
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }

  const findTab = (id: string) => tabs.find((t) => t.id === id) ?? null;

  // Terminal ANSI palette: dark = Catppuccin Mocha, light = Catppuccin Latte.
  // Follows settings.resolved, same source as the UI data-theme.
  const TERM_THEMES = {
    dark: {
      background: "#111113",
      foreground: "#cdd6f4",
      cursor: "#f5e0dc",
      cursorAccent: "#111113",
      selectionBackground: "#3a3a5a",
      black: "#45475a",
      red: "#f38ba8",
      green: "#a6e3a1",
      yellow: "#f9e2af",
      blue: "#89b4fa",
      magenta: "#cba6f7",
      cyan: "#94e2d5",
      white: "#bac2de",
      brightBlack: "#585b70",
      brightRed: "#f38ba8",
      brightGreen: "#a6e3a1",
      brightYellow: "#f9e2af",
      brightBlue: "#89b4fa",
      brightMagenta: "#cba6f7",
      brightCyan: "#94e2d5",
      brightWhite: "#cdd6f4",
    },
    light: {
      background: "#fcfcfd",
      foreground: "#4c4f69",
      cursor: "#dc8a78",
      cursorAccent: "#fcfcfd",
      selectionBackground: "#d0d2d9",
      black: "#5c5f77",
      red: "#d20f39",
      green: "#40a02b",
      yellow: "#df8e1d",
      blue: "#1e66f5",
      magenta: "#8839ef",
      cyan: "#179299",
      white: "#acb0be",
      brightBlack: "#6c6f85",
      brightRed: "#d20f39",
      brightGreen: "#40a02b",
      brightYellow: "#df8e1d",
      brightBlue: "#1e66f5",
      brightMagenta: "#8839ef",
      brightCyan: "#179299",
      brightWhite: "#4c4f69",
    },
  } as const;

  // Terminal mount: build xterm when a tab's .term element first appears, clean up on unmount.
  function attach(node: HTMLDivElement, id: string) {
    const term = new Terminal({
      fontFamily: 'Menlo, "SF Mono", Consolas, monospace',
      fontSize: settings.termFont,
      cursorBlink: true,
      theme: TERM_THEMES[settings.resolved],
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(node);
    // Keystrokes -> send to the session only when connected
    term.onData((d) => {
      const t = findTab(id);
      if (t?.status === "open") {
        invoke("ssh_write", {
          id,
          data: Array.from(new TextEncoder().encode(d)),
        });
      }
    });
    terms.set(id, { term, fit, unlisteners: [], lastCols: 0, lastRows: 0 });
    queueMicrotask(() => fitOne(id));

    return {
      destroy() {
        const b = terms.get(id);
        b?.unlisteners.forEach((u) => u());
        b?.term.dispose();
        terms.delete(id);
      },
    };
  }

  // resize dedup: notify remote window-change only when size actually changed, to avoid SIGWINCH storms
  function fitOne(id: string) {
    const b = terms.get(id);
    if (!b) return;
    try {
      b.fit.fit();
    } catch {
      return;
    }
    if (b.term.cols === b.lastCols && b.term.rows === b.lastRows) return;
    b.lastCols = b.term.cols;
    b.lastRows = b.term.rows;
    if (findTab(id)?.status === "open") {
      invoke("ssh_resize", { id, cols: b.term.cols, rows: b.term.rows });
    }
  }

  onMount(() => {
    // Async load + one-time migration (legacy plaintext password -> Keychain); fire-and-forget,
    // hosts.list refreshes once migration completes.
    hosts.load();
    const ro = new ResizeObserver(() => {
      clearTimeout(resizeTimer);
      resizeTimer = setTimeout(() => {
        if (activeId) fitOne(activeId);
      }, 80);
    });
    ro.observe(stackEl);

    // Global shortcuts: ⌘N add host / ⌘W close session / ⌘1-9 switch session.
    const onKey = (e: KeyboardEvent) => {
      if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
      if (e.key === "n") {
        e.preventDefault();
        openAdd();
      } else if (e.key === "w") {
        if (active) {
          e.preventDefault();
          closeTab(active);
        }
      } else if (e.key >= "1" && e.key <= "9") {
        const i = Number(e.key) - 1;
        if (tabs[i]) {
          e.preventDefault();
          setActive(tabs[i].id);
        }
      }
    };
    window.addEventListener("keydown", onKey);

    // Global host-key trust prompt: backend blocks the handshake and emits this on first connect
    // to an unknown host. Enqueue; the modal renders queue[0]. One listener for all sessions.
    let unlistenHostKey: UnlistenFn | undefined;
    listen<HostKeyPrompt>("ssh-hostkey-prompt", (e) => {
      hostKeyQueue = [...hostKeyQueue, e.payload];
    }).then((u) => {
      unlistenHostKey = u;
    });

    return () => {
      clearTimeout(resizeTimer);
      ro.disconnect();
      window.removeEventListener("keydown", onKey);
      unlistenHostKey?.();
    };
  });

  // Terminal font change -> apply to all xterm instances and refit the active terminal (triggers backend resize).
  // untrack: follow settings.termFont only, not activeId / terms changes.
  $effect(() => {
    const fs = settings.termFont;
    untrack(() => {
      for (const b of terms.values()) b.term.options.fontSize = fs;
      if (activeId) fitOne(activeId);
    });
  });

  // Theme mode change -> swap all xterm palettes (UI variables handled by CSS data-theme).
  $effect(() => {
    const theme = TERM_THEMES[settings.resolved];
    untrack(() => {
      for (const b of terms.values()) b.term.options.theme = theme;
    });
  });

  // Tab operations
  // Open a new tab. Connect directly if auto-connectable (key auth / saved password / typed password); else stay in form.
  async function openTab(src: {
    hostId?: string;
    label: string;
    host: string;
    port: number;
    user: string;
    password?: string;
    auth?: AuthKind;
    keyPath?: string;
    savePassword?: boolean;
  }) {
    const auth = src.auth ?? "password";
    const tab: Tab = {
      id: crypto.randomUUID(),
      kind: "ssh",
      hostId: src.hostId,
      label: src.label,
      host: src.host,
      port: src.port,
      user: src.user,
      password: src.password ?? "",
      auth,
      keyPath: src.keyPath ?? "",
      savePassword: src.savePassword ?? false,
      status: "form",
      msg: "",
    };
    tabs = [...tabs, tab];
    activeId = tab.id;
    await tick(); // wait for .term element to mount and xterm to build
    // Must use the proxy object from tabs, or state changes won't trigger UI updates
    const live = findTab(tab.id);
    // Key auth, saved password, or typed password -> connect directly; else wait for password input.
    if (live && (live.auth === "key" || live.savePassword || live.password)) {
      connectTab(live);
    } else {
      passEl?.focus();
    }
  }

  // Click a sidebar host: open a new tab (multiple per host allowed); connect directly if password saved
  function selectHost(h: Host) {
    selectedId = h.id;
    // Host uses field id; openTab/backend read Keychain by hostId, so map explicitly.
    openTab({ ...h, hostId: h.id });
  }

  // + on the tab bar: duplicate the current connection (reuses password, reconnects directly)
  function dupActive() {
    if (active?.kind === "ssh") openTab(active);
  }

  function setActive(id: string) {
    activeId = id;
    queueMicrotask(() => {
      fitOne(id);
      const t = findTab(id);
      if (t?.status === "open") terms.get(id)?.term.focus();
      else passEl?.focus();
    });
  }

  async function closeTab(tab: Tab, e?: MouseEvent) {
    e?.stopPropagation();
    if (tab.status === "open" || tab.status === "connecting") {
      try {
        await invoke("ssh_disconnect", { id: tab.id });
      } catch {
        // already gone, fine
      }
    }
    // If this tab has SFTP / monitor open, close them too
    if (sftpOpenId === tab.id) sftpOpenId = null;
    if (monitorOpenId === tab.id) monitorOpenId = null;
    const idx = tabs.findIndex((t) => t.id === tab.id);
    tabs = tabs.filter((t) => t.id !== tab.id);
    if (activeId === tab.id) {
      const next = tabs[idx] ?? tabs[idx - 1] ?? null;
      activeId = next ? next.id : null;
      if (next) setActive(next.id);
    }
  }

  // Connect / disconnect / reconnect
  async function connectTab(tab: Tab) {
    if (tab.status === "connecting" || tab.status === "open") return;
    if (!tab.host.trim()) {
      tab.msg = tr("msg.needHost");
      return;
    }
    const b = terms.get(tab.id);
    if (!b) return;

    tab.status = "connecting";
    tab.msg = tr("bar.connecting");

    // Reconnect: clear the previous listeners to avoid stacking
    b.unlisteners.forEach((u) => u());
    b.unlisteners = [];

    try {
      b.fit.fit();
    } catch {
      // ignore
    }
    b.lastCols = b.term.cols;
    b.lastRows = b.term.rows;

    // Terminal output: per-session Channel streams raw bytes; JS receives ArrayBuffer and feeds xterm
    // directly, skipping JSON number-array codec. A fresh Channel per connectTab; the old one dies with
    // its connection (no unlisten needed). onmessage closes over this tab's term, so no id filtering.
    // Set onmessage before invoke to not miss the earliest output.
    const ch = new Channel<ArrayBuffer>();
    ch.onmessage = (buf) => b.term.write(new Uint8Array(buf));

    // Tab still alive: still in tabs (not removed by closeTab) and the same term bundle (not destroyed
    // and rebuilt by #each). The SSH handshake takes seconds, during which the user may close the tab;
    // writing tab.status / b.term.* after that hits a destroyed object/terminal, so revalidate after each await.
    const stillLive = () => findTab(tab.id) !== null && terms.get(tab.id) === b;

    // ssh-closed is still a global (low-frequency) event; attach listener before connecting, filter by id, store handle in unlisteners.
    const closed = await listen<{ id: string }>("ssh-closed", (e) => {
      if (e.payload.id === tab.id) {
        tab.status = "closed";
        tab.msg = tr("msg.closed");
        b.term.write(`\r\n\x1b[33m${tr("term.closed")}\x1b[0m\r\n`);
        // On disconnect, collapse this tab's SFTP / monitor panels (backend lookups would fail).
        if (sftpOpenId === tab.id) sftpOpenId = null;
        if (monitorOpenId === tab.id) monitorOpenId = null;
      }
    });
    // If the tab was closed during the listen await: don't push (destroy already ran unlisteners, so the
    // handle would be an orphan -> leaked global listener + closure pinning a disposed term). Unlisten and exit.
    if (!stillLive()) {
      closed();
      return;
    }
    b.unlisteners.push(closed);

    try {
      // Build args.auth by auth method.
      //   Password mode: send typed value; if empty and savePassword, backend reads Keychain.
      //   Key mode: path + transient passphrase (collected via password field, not persisted).
      const authArg =
        tab.auth === "key"
          ? { type: "key", path: tab.keyPath.trim(), passphrase: tab.password || undefined }
          : { type: "password", password: tab.password || undefined };
      await invoke("ssh_connect", {
        args: {
          id: tab.id,
          hostId: tab.hostId,
          host: tab.host.trim(),
          port: Number(tab.port),
          user: tab.user.trim(),
          savePassword: tab.savePassword,
          auth: authArg,
          cols: b.term.cols,
          rows: b.term.rows,
        },
        // onOutput -> backend on_output (camelCase mapping): this session's output Channel
        onOutput: ch,
      });
      // Connected, but the tab may have closed during the await: backend session is up, so disconnect to
      // reclaim it, and don't write the destroyed tab/term (else ghost open state + writing a disposed terminal).
      if (!stillLive()) {
        invoke("ssh_disconnect", { id: tab.id }).catch(() => {});
        return;
      }
      tab.status = "open";
      tab.msg = "";
      b.term.focus();
    } catch (err) {
      // On failure return to form; confirm the tab still exists to avoid writing a closed tab/disposed terminal.
      if (!stillLive()) return;
      tab.status = "form";
      tab.msg = String(err);
      b.term.write(`\r\n\x1b[31m${err}\x1b[0m\r\n`);
      queueMicrotask(() => passEl?.focus());
    }
  }

  async function disconnectTab(tab: Tab) {
    await invoke("ssh_disconnect", { id: tab.id });
    // The actual state flip is handled by the ssh-closed event
  }

  // Reconnect: connect directly if key auth / saved password / password still present, else return to form
  function reopen(tab: Tab) {
    if (tab.auth === "key" || tab.savePassword || tab.password) {
      connectTab(tab);
    } else {
      tab.status = "form";
      tab.msg = "";
      queueMicrotask(() => passEl?.focus());
    }
  }

  // Host library: add / edit / delete
  // Add/edit both use one dedicated tab (single global form tab, reused and brought to front).
  function openFormTab() {
    let ft = tabs.find((t) => t.kind === "host-form");
    if (!ft) {
      ft = {
        id: crypto.randomUUID(),
        kind: "host-form",
        label: "",
        host: "",
        port: 22,
        user: "",
        password: "",
        auth: "password",
        keyPath: "",
        savePassword: false,
        status: "form",
        msg: "",
      };
      tabs = [...tabs, ft];
    }
    // label is no longer hardcoded: tab title / top bar derive from kind via tr(), following language switches.
    activeId = ft.id;
  }

  function closeFormTab() {
    const ft = tabs.find((t) => t.kind === "host-form");
    if (ft) closeTab(ft);
  }

  // Settings tab (global singleton, reused and brought to front).
  function openSettings() {
    let st = tabs.find((t) => t.kind === "settings");
    if (!st) {
      st = {
        id: crypto.randomUUID(),
        kind: "settings",
        label: "",
        host: "",
        port: 22,
        user: "",
        password: "",
        auth: "password",
        keyPath: "",
        savePassword: false,
        status: "form",
        msg: "",
      };
      tabs = [...tabs, st];
    }
    activeId = st.id;
  }

  function openAdd() {
    formEditId = null;
    fLabel = "";
    fHost = "";
    fPort = 22;
    fUser = "root";
    fPass = "";
    fAuth = "password";
    fKeyPath = "";
    fSavePass = false;
    openFormTab();
  }

  function openEdit(h: Host, e: MouseEvent) {
    e.stopPropagation();
    formEditId = h.id;
    fLabel = h.label;
    fHost = h.host;
    fPort = h.port;
    fUser = h.user;
    fPass = ""; // password not read back from Keychain; empty means "unchanged"
    fAuth = h.auth ?? "password";
    fKeyPath = h.keyPath ?? "";
    fSavePass = h.savePassword ?? false;
    openFormTab();
  }

  async function saveForm() {
    if (!fHost.trim()) return;
    // In edit mode an empty password field = "keep password": need to know if this host already had a saved
    // password, else editing only the label flips savePassword false and wrongly deletes the Keychain entry.
    const prev = formEditId ? hosts.list.find((h) => h.id === formEditId) : null;
    const hadSaved = prev?.savePassword ?? false;
    // Whether to save the password: password mode + save checked + (new password typed, or editing one already saved and still checked).
    const keepSaved = fAuth === "password" && fSavePass && (!!fPass || hadSaved);
    // DB stores metadata + flags only, never plaintext passwords.
    const data = {
      label: fLabel.trim() || fHost.trim(),
      host: fHost.trim(),
      port: Number(fPort),
      user: fUser.trim() || "root",
      auth: fAuth,
      keyPath: fAuth === "key" ? fKeyPath.trim() : undefined,
      savePassword: keepSaved,
    };
    let id: string;
    if (formEditId) {
      await hosts.update(formEditId, data);
      id = formEditId;
    } else {
      id = (await hosts.add(data)).id;
    }

    try {
      if (fAuth === "password" && fSavePass && fPass) {
        // New password typed and save checked -> overwrite Keychain (not localStorage).
        await invoke("secret_set", { hostId: id, password: fPass });
      } else if (fAuth === "password" && fSavePass && !fPass && hadSaved) {
        // Edit: save checked but password field empty -> keep the existing Keychain value, do nothing.
      } else if (fAuth === "password") {
        // Unchecked, or checked with nothing to save -> clear any existing entry (idempotent).
        await invoke("secret_delete", { hostId: id });
      }
    } catch {
      // Keychain failure doesn't block saving metadata; connect prompts for manual input
    }
    closeFormTab();
  }

  async function removeHost(h: Host, e: MouseEvent) {
    e.stopPropagation();
    try {
      await invoke("secret_delete", { hostId: h.id }); // remove the matching Keychain entry
    } catch {
      // ignore: failing to delete doesn't block host removal
    }
    await hosts.remove(h.id);
    if (selectedId === h.id) selectedId = null;
  }

  function onPassKey(e: KeyboardEvent) {
    if (e.key === "Enter" && active) connectTab(active);
  }
  function onFormKey(e: KeyboardEvent) {
    if (e.key === "Enter") saveForm();
    if (e.key === "Escape") closeFormTab();
  }
</script>

<!-- Host icon: color by session status (green=connected / yellow=connecting / red=closed / gray=idle) -->
{#snippet hostIcon(status: Status)}
  <svg
    class="hicon {status}"
    width="15"
    height="15"
    viewBox="0 0 16 16"
    fill="none"
    stroke="currentColor"
    stroke-width="1.3"
    aria-hidden="true"
  >
    <rect x="2" y="2.5" width="12" height="4.5" rx="1.2" />
    <rect x="2" y="9" width="12" height="4.5" rx="1.2" />
    <circle cx="4.6" cy="4.75" r="0.85" fill="currentColor" stroke="none" />
    <circle cx="4.6" cy="11.25" r="0.85" fill="currentColor" stroke="none" />
  </svg>
{/snippet}

{#snippet folderIcon()}
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" aria-hidden="true">
    <path d="M2 4.5a1 1 0 0 1 1-1h3l1.4 1.5H13a1 1 0 0 1 1 1v5.5a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V4.5Z" />
  </svg>
{/snippet}

{#snippet chartIcon()}
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="M2 2v11.5a.5.5 0 0 0 .5.5H14M5 10V8M8 10V5M11 10V7" />
  </svg>
{/snippet}

{#snippet plugIcon()}
  <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="M6 2v3M10 2v3M4.5 5h7v2.5a3.5 3.5 0 0 1-7 0V5ZM8 11v3" />
  </svg>
{/snippet}

{#snippet gearIcon()}
  <svg class="hicon form" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="M10.4 2.2a3.2 3.2 0 0 0-3.8 4.1L2.3 10.6a1.3 1.3 0 0 0 1.8 1.8l4.3-4.3a3.2 3.2 0 0 0 4.1-3.8L10.6 6 9 6 9 4.4l1.4-2.2Z" />
  </svg>
{/snippet}

{#snippet searchIcon()}
  <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" aria-hidden="true">
    <circle cx="7" cy="7" r="4.5" />
    <path d="M10.5 10.5 14 14" />
  </svg>
{/snippet}

{#snippet editIcon()}
  <svg class="hicon form" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
    <path d="M11 2.5 13.5 5 6 12.5l-3 .5.5-3L11 2.5Z" />
  </svg>
{/snippet}

{#snippet modeIcon(m: ThemeMode)}
  {#if m === "light"}
    <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <circle cx="8" cy="8" r="3" />
      <path d="M8 1v1.5M8 13.5V15M1 8h1.5M13.5 8H15M3 3l1 1M12 12l1 1M13 3l-1 1M4 12l-1 1" />
    </svg>
  {:else if m === "dark"}
    <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M13.5 9.2A5.5 5.5 0 0 1 6.8 2.5 5.5 5.5 0 1 0 13.5 9.2Z" />
    </svg>
  {:else}
    <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <circle cx="8" cy="8" r="6" />
      <path d="M8 2a6 6 0 0 0 0 12Z" fill="currentColor" stroke="none" />
    </svg>
  {/if}
{/snippet}

<div class="app">
  <!-- Top drag strip: covers the native title bar so the traffic-light buttons sit on a body-colored dark base -->
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="titlebar" data-tauri-drag-region></div>
  <div class="workspace">
  <aside class="sidebar" style="width: {sidebarW}px">
    <div class="head">
      <span class="brand"><span class="mark"></span>Moonshell</span>
      <div class="head-actions">
        <button class="add ico" onclick={openSettings} title={tr("sidebar.settings")} aria-label={tr("sidebar.settings")}>{@render gearIcon()}</button>
        <button class="add" onclick={openAdd} title={tr("sidebar.addHost")} aria-label={tr("sidebar.addHost")}>+</button>
      </div>
    </div>
    {#if hosts.list.length > 5}
      <div class="search">
        {@render searchIcon()}
        <input
          class="sfield"
          placeholder={tr("sidebar.search")}
          bind:value={hostFilter}
          spellcheck="false"
        />
        {#if hostFilter}
          <button class="sclear" title={tr("common.clear")} onclick={() => (hostFilter = "")}>×</button>
        {/if}
      </div>
    {/if}
    <ul class="hosts">
      {#each filteredHosts as h (h.id)}
        {@const st = hostStatus.get(h.id)}
        <li class="host" class:active={selectedId === h.id}>
          <button class="pick" onclick={() => selectHost(h)} title={tr("sidebar.connectTitle", { target: `${h.user}@${h.host}:${h.port}` })}>
            <span class="dot {st ?? 'idle'}" title={st === 'open' ? tr('status.connected') : st === 'connecting' ? tr('status.connecting') : tr('status.disconnected')}></span>
            <span class="meta">
              <span class="label">{h.label}</span>
              <span class="sub">{h.user}@{h.host}:{h.port}</span>
            </span>
          </button>
          <button class="icon" title={tr("common.edit")} onclick={(e) => openEdit(h, e)}>✎</button>
          <button class="icon del" title={tr("common.delete")} onclick={(e) => removeHost(h, e)}>×</button>
        </li>
      {/each}
      {#if hosts.list.length === 0}
        <li class="empty">
          <span class="ehint">{tr("sidebar.empty")}</span>
          <button class="ebtn" onclick={openAdd}>{tr("sidebar.addFirst")}</button>
        </li>
      {:else if filteredHosts.length === 0}
        <li class="empty"><span class="ehint">{tr("sidebar.noMatch", { q: hostFilter })}</span></li>
      {/if}
    </ul>
  </aside>

  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="resizer"
    onpointerdown={(e) => startDrag(e, "sidebar")}
    role="separator"
    aria-orientation="vertical"
    title={tr("common.resize")}
  ></div>

  <main class="main">
    {#if tabs.length}
      <div class="tabs">
        {#each tabs as t (t.id)}
          <div
            class="tab"
            class:active={t.id === activeId}
            onclick={() => setActive(t.id)}
            role="button"
            tabindex="0"
            onkeydown={(e) => e.key === "Enter" && setActive(t.id)}
            title={t.kind === "ssh" ? `${t.user}@${t.host}:${t.port}` : t.kind === "settings" ? tr("settings.title") : tr(formEditId ? "form.editTitle" : "form.addTitle")}
          >
            {#if t.kind === "settings"}{@render gearIcon()}{:else if t.kind === "host-form"}{@render editIcon()}{:else}{@render hostIcon(t.status)}{/if}
            <span class="ttl">{t.kind === "ssh" ? t.label : t.kind === "settings" ? tr("settings.title") : tr(formEditId ? "form.editTitle" : "form.addTitle")}</span>
            <button class="x" title={tr("common.close")} onclick={(e) => closeTab(t, e)}>×</button>
          </div>
        {/each}
        <button class="newtab" onclick={dupActive} title={tr("tabs.newSession")}>+</button>
      </div>
    {/if}

    <!-- Settings / host-form top bar just repeats the tab icon+title, so skip it;
         show the top bar only for SSH sessions (connect/password/disconnect actions) and the no-tab hint. -->
    {#if active?.kind !== "settings" && active?.kind !== "host-form"}
    <header class="bar">
      {#if active}
        {@render hostIcon(active.status)}
        <span class="target">{active.user}@{active.host}:{active.port}</span>
        {#if active.status === "form"}
          {#if (active.password || active.savePassword || active.auth === "key") && !active.msg}
            <!-- Key auth / saved password / typed password: auto-connecting, no password field needed -->
            <span class="status">{tr("bar.connecting")}</span>
          {:else}
            <input
              class="f pass"
              type="password"
              placeholder={active.auth === "key" ? tr("bar.keyPassphrase") : tr("bar.password")}
              bind:value={active.password}
              bind:this={passEl}
              onkeydown={onPassKey}
            />
            <button class="btn" onclick={() => connectTab(active)}>{tr("bar.connect")}</button>
          {/if}
        {:else if active.status === "connecting"}
          <span class="status">{tr("bar.connecting")}</span>
        {:else if active.status === "open"}
          <span class="livedot" title={tr("status.connected")}></span>
          <span class="spacer"></span>
          <button
            class="btn ghost"
            class:on={sftpOpenId === active.id}
            onclick={() => (sftpOpenId = sftpOpenId === active.id ? null : active.id)}
            title={tr("bar.filesTitle")}
          >{@render folderIcon()}<span>{tr("bar.files")}</span></button>
          <button
            class="btn ghost"
            class:on={monitorOpenId === active.id}
            onclick={() => (monitorOpenId = monitorOpenId === active.id ? null : active.id)}
            title={tr("bar.monitorTitle")}
          >{@render chartIcon()}<span>{tr("bar.monitor")}</span></button>
          <button class="btn danger" onclick={() => disconnectTab(active)} title={tr("bar.disconnectTitle")}
            >{@render plugIcon()}<span>{tr("bar.disconnect")}</span></button>
        {:else}
          <span class="spacer"></span>
          <button class="btn ghost" onclick={() => reopen(active)}>{tr("bar.reconnect")}</button>
        {/if}
        {#if active.msg}<span class="status">{active.msg}</span>{/if}
      {:else}
        <span class="hint">{tr("bar.hint")}</span>
      {/if}
    </header>
    {/if}

    <div class="body">
      <div class="stack" bind:this={stackEl}>
        {#each tabs as t (t.id)}
          {#if t.kind === "ssh"}
            <div class="term" use:attach={t.id} hidden={t.id !== activeId}></div>
          {/if}
        {/each}

        {#if active?.kind === "host-form"}
          <div class="form-panel">
            <div class="form-card">
              <h2>{tr(formEditId ? "form.editTitle" : "form.addTitle")}</h2>

              <label class="fld">
                <span>{tr("form.name")}</span>
                <input class="f" placeholder={tr("form.namePh")} bind:value={fLabel} onkeydown={onFormKey} />
              </label>

              <label class="fld">
                <span>{tr("form.host")}</span>
                <input class="f" placeholder={tr("form.hostPh")} bind:value={fHost} onkeydown={onFormKey} />
              </label>

              <div class="frow">
                <label class="fld port">
                  <span>{tr("form.port")}</span>
                  <input class="f" type="number" placeholder="22" bind:value={fPort} onkeydown={onFormKey} />
                </label>
                <label class="fld user">
                  <span>{tr("form.user")}</span>
                  <input class="f" placeholder="root" bind:value={fUser} onkeydown={onFormKey} />
                </label>
              </div>

              <label class="fld">
                <span>{tr("form.auth")}</span>
                <select class="f" bind:value={fAuth}>
                  <option value="password">{tr("form.authPassword")}</option>
                  <option value="key">{tr("form.authKey")}</option>
                </select>
              </label>

              {#if fAuth === "key"}
                <label class="fld">
                  <span>{tr("form.keyPath")}</span>
                  <input class="f" placeholder={tr("form.keyPathPh")} bind:value={fKeyPath} onkeydown={onFormKey} />
                </label>
              {:else}
                <label class="fld">
                  <span>{tr("form.password")}</span>
                  <input
                    class="f"
                    type="password"
                    placeholder={formEditId ? tr("form.passwordPhEdit") : tr("form.passwordPhAdd")}
                    bind:value={fPass}
                    onkeydown={onFormKey}
                  />
                </label>
                <label class="chk" title={tr("form.savePassTitle")}>
                  <input type="checkbox" bind:checked={fSavePass} />
                  {tr("form.savePass")}
                </label>
              {/if}

              <div class="form-actions">
                <button class="btn" onclick={saveForm}>{tr(formEditId ? "form.save" : "form.add")}</button>
                <button class="btn ghost" onclick={closeFormTab}>{tr("form.cancel")}</button>
              </div>
            </div>
          </div>
        {/if}

        {#if active?.kind === "settings"}
          <div class="form-panel">
            <div class="form-card settings-card">
              <h2>{tr("settings.title")}</h2>

              <section class="set-group">
                <div class="set-head">
                  <span class="set-title">{tr("settings.language")}</span>
                  <span class="set-sub">{tr("settings.languageSub")}</span>
                </div>
                <div class="lang-grid">
                  <button
                    class="lang"
                    class:on={i18n.pref === "auto"}
                    onclick={() => i18n.set("auto")}
                    title={tr("settings.langAutoSub")}
                  >
                    <span class="lang-native">{tr("settings.langAuto")}</span>
                    <span class="lang-en">{LOCALES.find((l) => l.code === i18n.locale)?.native ?? ""}</span>
                    {#if i18n.pref === "auto"}
                      <svg class="lang-check" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m3.5 8.5 3 3 6-7" /></svg>
                    {/if}
                  </button>
                  {#each LOCALES as l (l.code)}
                    <button
                      class="lang"
                      class:on={i18n.pref === l.code}
                      onclick={() => i18n.set(l.code)}
                    >
                      <span class="lang-native">{l.native}</span>
                      <span class="lang-en">{l.en}</span>
                      {#if i18n.pref === l.code}
                        <svg class="lang-check" width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m3.5 8.5 3 3 6-7" /></svg>
                      {/if}
                    </button>
                  {/each}
                </div>
              </section>

              <section class="set-group">
                <div class="set-head">
                  <span class="set-title">{tr("settings.mode")}</span>
                  <span class="set-sub">{tr("settings.modeSub")}</span>
                </div>
                <div class="seg">
                  {#each MODES as m (m.key)}
                    <button
                      class="seg-btn"
                      class:on={settings.mode === m.key}
                      onclick={() => settings.setMode(m.key)}
                    >
                      {@render modeIcon(m.key)}
                      <span>{tr(m.label)}</span>
                    </button>
                  {/each}
                </div>
              </section>

              <section class="set-group">
                <div class="set-head">
                  <span class="set-title">{tr("settings.theme")}</span>
                  <span class="set-sub">{tr("settings.themeSub")}</span>
                </div>
                <div class="swatches">
                  {#each ACCENTS as a (a.key)}
                    <button
                      class="swatch"
                      class:on={settings.accent === a.key}
                      style="--sw: {a.hex}"
                      onclick={() => settings.setAccent(a.key)}
                      aria-label={a.key}
                      title={a.key}
                    ></button>
                  {/each}
                </div>
              </section>

              <section class="set-group">
                <div class="set-head">
                  <span class="set-title">{tr("settings.fontSize")}</span>
                  <span class="set-sub">{tr("settings.fontSizeSub")}</span>
                </div>
                <div class="stepper">
                  <button
                    class="step"
                    onclick={() => settings.setTermFont(settings.termFont - 1)}
                    disabled={settings.termFont <= TERM_FONT_MIN}
                    aria-label="-"
                  >−</button>
                  <span class="step-val">{settings.termFont} px</span>
                  <button
                    class="step"
                    onclick={() => settings.setTermFont(settings.termFont + 1)}
                    disabled={settings.termFont >= TERM_FONT_MAX}
                    aria-label="+"
                  >+</button>
                </div>
              </section>
            </div>
          </div>
        {/if}

        {#if active && active.kind === "ssh" && active.status === "form" && active.auth === "password" && !active.password && !active.savePassword && !active.msg}
          <div class="overlay">
            <div class="empty-art">{@render hostIcon(active.status)}</div>
            <div class="empty-title">{active.user}@{active.host}</div>
            <div class="empty-sub">{tr("overlay.enterPassword")}</div>
          </div>
        {/if}
        {#if tabs.length === 0}
          <div class="welcome">
            <div class="empty-art">{@render hostIcon('form')}</div>
            <div class="empty-title">{tr("welcome.title")}</div>
            <div class="empty-sub">{tr("welcome.sub")}</div>
          </div>
        {/if}
      </div>
      {#if sftpOpenId}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="resizer"
          onpointerdown={(e) => startDrag(e, "sftp")}
          role="separator"
          aria-orientation="vertical"
          title={tr("common.resize")}
        ></div>
        {#key sftpOpenId}
          <Sftp
            sessionId={sftpOpenId}
            title={sftpTitle}
            width={sftpW}
            onClose={() => (sftpOpenId = null)}
          />
        {/key}
      {/if}
      {#if monitorOpenId}
        <!-- svelte-ignore a11y_no_static_element_interactions -->
        <div
          class="resizer"
          onpointerdown={(e) => startDrag(e, "monitor")}
          role="separator"
          aria-orientation="vertical"
          title={tr("common.resize")}
        ></div>
        {#key monitorOpenId}
          <Monitor
            sessionId={monitorOpenId}
            title={monitorTitle}
            width={monitorW}
            onClose={() => (monitorOpenId = null)}
          />
        {/key}
      {/if}
    </div>
  </main>
  </div>

  {#if hostKeyPrompt}
    <div class="hk-backdrop" role="dialog" aria-modal="true">
      <div class="hk-card">
        <div class="hk-title">{tr("hostkey.title")}</div>
        <div class="hk-intro">
          {tr("hostkey.intro", {
            target: `${hostKeyPrompt.host}:${hostKeyPrompt.port}`,
          })}
        </div>
        <div class="hk-row">
          <span class="hk-label">{tr("hostkey.algo")}</span>
          <span class="hk-val">{hostKeyPrompt.algorithm}</span>
        </div>
        <div class="hk-row">
          <span class="hk-label">{tr("hostkey.fingerprint")}</span>
          <span class="hk-val hk-fp">{hostKeyPrompt.fingerprint}</span>
        </div>
        <div class="hk-warn">{tr("hostkey.warn")}</div>
        <div class="hk-actions">
          <button class="hk-btn" onclick={() => decideHostKey(false)}>
            {tr("hostkey.reject")}
          </button>
          <button class="hk-btn hk-trust" onclick={() => decideHostKey(true)}>
            {tr("hostkey.trust")}
          </button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  :global(:root) {
    /* Radix Slate (dark) neutral scale; lighter on top for depth.
       Scale: 1 base / 2 sidebar+top bar / 3 card / 4 hover / 5-6 lines / 7 strong border /
       9-10 faint text / 11 secondary text / 12 primary text. Accents use Catppuccin. */
    --bg: #111113;          /* Slate 1 */
    --surface-1: #18191b;   /* Slate 2: sidebar / top bar */
    --surface-2: #26282b;   /* Slate 4: hover / active tab / selected host */
    --surface-3: #1d1e20;   /* inset: segment track / stepper */
    --card: #212225;        /* Slate 3: raised card */
    --term-bg: #111113;
    --line: #2a2c30;        /* Slate 5/6: thin line */
    --line-strong: #3a3d42; /* Slate 7: interactive / strong border */
    --scroll: #3a3d42;      /* scrollbar thumb */
    --text: #ededef;        /* Slate 12 */
    --text-dim: #b0b4ba;    /* Slate 11 */
    --text-mute: #6e727a;   /* Slate 9/10 */
    /* Accents: status and focus only */
    --blue: #89b4fa;
    --green: #a6e3a1;
    --yellow: #f9e2af;
    --red: #f38ba8;
    --pink: #f5e0dc;
    --blue-soft: rgba(137, 180, 250, 0.12);
    --red-soft: rgba(243, 139, 168, 0.12);
    /* Spacing / radius / font size */
    --r-sm: 6px;
    --r-md: 9px;
    --focus-ring: 0 0 0 2px rgba(137, 180, 250, 0.35);
    /* Card shadow */
    --shadow-card: 0 1px 2px rgba(0, 0, 0, 0.4), 0 8px 22px rgba(0, 0, 0, 0.28);
  }
  /* Light theme (Catppuccin Latte): overrides base scale and neutrals; accents stay via inline accent variables. */
  :global([data-theme="light"]) {
    /* Radix Slate (light) neutral scale + pure white cards. Terminal stays Latte; only the shell is neutralized. */
    --bg: #fcfcfd;           /* Slate 1: main content / terminal frame */
    --surface-1: #f9f9fb;     /* Slate 2: sidebar / top bar */
    --surface-2: #f0f0f3;     /* Slate 3: hover / active tab / input base */
    --surface-3: #e8e8ec;     /* Slate 4: inset (segment track / stepper) */
    --card: #ffffff;          /* raised card surface */
    --term-bg: #fcfcfd;
    --line: #e2e3e8;          /* Slate 5/6: neutral thin line */
    --line-strong: #d0d2d9;   /* Slate 7 */
    --scroll: #c4c6cf;        /* scrollbar thumb */
    --text: #1c2024;          /* Slate 12 */
    --text-dim: #60646c;      /* Slate 11 */
    --text-mute: #8b8d98;     /* Slate 9 */
    --green: #40a02b;
    --yellow: #df8e1d;
    --red: #d20f39;
    --pink: #ea76cb;
    --red-soft: rgba(210, 15, 57, 0.1);
    /* Very light neutral shadow + thin border to define card edges */
    --shadow-card: 0 1px 2px rgba(16, 24, 40, 0.04),
      0 4px 12px rgba(16, 24, 40, 0.06);
  }
  :global(html), :global(body) {
    margin: 0;
    height: 100%;
    /* Desktop shell: root doesn't scroll, only inner panels (terminal viewport / sidebar / settings) do,
       else a window-level scrollbar shows a dark track on the right. */
    overflow: hidden;
    background: var(--bg);
    -webkit-font-smoothing: antialiased;
  }
  :global(*) { box-sizing: border-box; }
  /* Style all scrollbars: transparent track and corner to hide the dark default track in any theme */
  :global(::-webkit-scrollbar) { width: 11px; height: 11px; background: transparent; }
  :global(::-webkit-scrollbar-track) { background: transparent; }
  :global(::-webkit-scrollbar-corner) { background: transparent; }
  :global(::-webkit-scrollbar-thumb) {
    background: var(--scroll);
    border-radius: 8px;
    border: 3px solid transparent;
    background-clip: padding-box;
  }
  :global(::-webkit-scrollbar-thumb:hover) { background: var(--text-mute); background-clip: padding-box; }
  .app {
    display: flex;
    flex-direction: column;
    height: 100vh;
    overflow: hidden;
    font-family: system-ui, "PingFang SC", sans-serif;
    color: var(--text);
  }
  /* Title bar drag strip: body-colored, traffic-light buttons float on it */
  .titlebar {
    flex-shrink: 0;
    height: 28px;
    background: var(--bg);
  }
  .workspace {
    flex: 1;
    min-height: 0;
    display: flex;
  }

  /* Sidebar */
  .sidebar {
    width: 245px;
    flex-shrink: 0;
    background: var(--surface-1);
    border-right: 1px solid var(--line);
    display: flex;
    flex-direction: column;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 14px;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 700;
    font-size: 16px;
    color: var(--pink);
    letter-spacing: 1.5px;
  }
  .mark {
    width: 8px;
    height: 8px;
    border-radius: 2px;
    background: linear-gradient(135deg, var(--blue), var(--pink));
    box-shadow: 0 0 8px rgba(137, 180, 250, 0.45);
  }
  .add {
    display: flex;
    align-items: center;
    justify-content: center;
    background: var(--surface-2);
    color: var(--blue);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    width: 26px;
    height: 26px;
    font-size: 17px;
    line-height: 1;
    cursor: pointer;
    transition: background 0.12s, border-color 0.12s, color 0.12s;
  }
  .add:hover { border-color: var(--blue); background: var(--surface-3); }
  .add:active { transform: translateY(0.5px); }
  .head-actions { display: flex; align-items: center; gap: 6px; }
  /* Icon button (settings): same size as +, holds an svg */
  .add.ico { padding: 0; }

  .search {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 0 10px 6px;
    padding: 0 8px;
    height: 30px;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    color: var(--text-mute);
  }
  .search:focus-within { border-color: var(--blue); box-shadow: var(--focus-ring); }
  .sfield {
    flex: 1;
    min-width: 0;
    background: none;
    border: none;
    outline: none;
    color: var(--text);
    font-size: 13px;
  }
  .sfield::placeholder { color: var(--text-mute); }
  .sclear {
    background: none;
    border: none;
    color: var(--text-mute);
    cursor: pointer;
    font-size: 15px;
    line-height: 1;
    padding: 0 2px;
  }
  .sclear:hover { color: var(--text); }

  .hosts {
    list-style: none;
    margin: 0;
    padding: 4px 8px 8px;
    overflow-y: auto;
    flex: 1;
  }
  .host {
    position: relative;
    display: flex;
    align-items: center;
    border-radius: var(--r-sm);
    border: 1px solid transparent;
    margin-bottom: 2px;
    transition: background 0.12s, border-color 0.12s;
  }
  .host:hover { background: var(--surface-2); }
  .host.active {
    background: var(--blue-soft);
    border-color: color-mix(in srgb, var(--blue) 30%, transparent);
  }
  .host.active::before {
    content: "";
    position: absolute;
    left: 0;
    top: 7px;
    bottom: 7px;
    width: 3px;
    border-radius: 0 2px 2px 0;
    background: var(--blue);
  }
  .host.active .label { color: var(--blue); }
  .pick {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 9px;
    background: none;
    border: none;
    padding: 8px 9px;
    cursor: pointer;
    text-align: left;
  }
  .dot {
    flex-shrink: 0;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--surface-3);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.06);
  }
  .dot.open { background: var(--green); box-shadow: 0 0 6px rgba(166, 227, 161, 0.6); }
  .dot.connecting { background: var(--yellow); animation: pulse 1.2s ease-in-out infinite; }
  .meta {
    display: flex;
    flex-direction: column;
    gap: 1px;
    min-width: 0;
  }
  .label {
    color: var(--text);
    font-size: 14px;
    font-weight: 600;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
  }
  .sub {
    color: var(--text-mute);
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
  }
  .icon {
    background: none;
    border: none;
    color: var(--text-mute);
    cursor: pointer;
    font-size: 14px;
    padding: 4px 6px;
    border-radius: 4px;
    opacity: 0;
    transition: opacity 0.12s, color 0.12s, background 0.12s;
  }
  .host:hover .icon { opacity: 1; }
  .icon:hover { color: var(--text); background: var(--surface-3); }
  .icon.del:hover { color: var(--red); }
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
    color: var(--text-mute);
    font-size: 13px;
    padding: 24px 12px;
    text-align: center;
  }
  .ehint { color: var(--text-mute); }
  .ebtn {
    background: var(--surface-2);
    color: var(--blue);
    border: 1px solid var(--line-strong);
    border-radius: var(--r-sm);
    padding: 7px 12px;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
    transition: background 0.12s, border-color 0.12s;
  }
  .ebtn:hover { border-color: var(--blue); background: var(--surface-3); }

  /* Drag separator: invisible at rest, edge on hover, highlighted while dragging */
  .resizer {
    flex-shrink: 0;
    width: 5px;
    cursor: col-resize;
    background: transparent;
    transition: background 0.12s;
  }
  .resizer:hover { background: var(--blue); }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50% { opacity: 0.35; }
  }

  /* Main area */
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
  }

  /* Tab bar */
  .tabs {
    display: flex;
    align-items: stretch;
    gap: 4px;
    padding: 8px 8px 0;
    background: var(--bg);
    overflow-x: auto;
    scrollbar-width: none;
  }
  .tabs::-webkit-scrollbar { height: 0; }
  .tab {
    position: relative;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 7px 9px 7px 11px;
    background: transparent;
    border: 1px solid transparent;
    border-bottom: none;
    border-radius: var(--r-sm) var(--r-sm) 0 0;
    cursor: pointer;
    max-width: 190px;
    color: var(--text-mute);
    transition: background 0.12s, color 0.12s;
  }
  .tab:hover { background: var(--surface-2); color: var(--text-dim); }
  .tab.active {
    background: var(--term-bg);
    color: var(--text);
    border-color: var(--line);
  }
  .tab.active::before {
    content: "";
    position: absolute;
    left: -1px;
    right: -1px;
    top: 0;
    height: 2px;
    border-radius: 2px 2px 0 0;
    background: var(--blue);
  }
  .tab .ttl {
    font-size: 13px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .x {
    background: none;
    border: none;
    color: var(--text-mute);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    padding: 1px 3px;
    border-radius: 4px;
    transition: color 0.12s, background 0.12s;
  }
  .x:hover { color: var(--red); background: var(--surface-3); }
  .newtab {
    background: none;
    border: none;
    color: var(--text-mute);
    cursor: pointer;
    font-size: 18px;
    padding: 0 10px;
    align-self: center;
    border-radius: 4px;
    transition: color 0.12s;
  }
  .newtab:hover { color: var(--blue); }

  /* Host icon: color encodes status */
  .hicon {
    flex-shrink: 0;
    color: var(--text-mute);
  }
  .hicon.open { color: var(--green); }
  .hicon.connecting { color: var(--yellow); animation: pulse 1.2s ease-in-out infinite; }
  .hicon.closed { color: var(--red); }
  .hicon.form { color: var(--blue); }

  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 14px;
    background: var(--surface-1);
    border-bottom: 1px solid var(--line);
    min-height: 38px;
  }
  .spacer { flex: 1; }
  .target {
    color: var(--text);
    font-size: 14px;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
  }
  .livedot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--green);
    box-shadow: 0 0 6px rgba(166, 227, 161, 0.6);
  }
  .hint {
    color: var(--text-mute);
    font-size: 14px;
  }
  .f {
    background: var(--surface-2);
    border: 1px solid var(--line);
    color: var(--text);
    border-radius: var(--r-sm);
    padding: 7px 10px;
    font-size: 14px;
    outline: none;
    transition: border-color 0.12s, box-shadow 0.12s;
  }
  .f::placeholder { color: var(--text-mute); }
  .f:focus { border-color: var(--blue); box-shadow: var(--focus-ring); }
  select.f { cursor: pointer; }
  .chk {
    display: flex;
    align-items: center;
    gap: 5px;
    color: var(--text-dim);
    font-size: 13px;
    cursor: pointer;
    user-select: none;
  }
  .chk input { cursor: pointer; accent-color: var(--blue); }
  .pass { width: 160px; }
  .btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    background: var(--blue);
    color: var(--bg);
    border: 1px solid transparent;
    border-radius: var(--r-sm);
    padding: 0 16px;
    height: 30px;
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
    transition: background 0.12s, border-color 0.12s, color 0.12s, filter 0.12s;
  }
  .btn:hover { filter: brightness(1.06); }
  .btn:active { transform: translateY(0.5px); filter: brightness(0.95); }
  .btn:disabled { opacity: 0.5; cursor: default; filter: none; }
  /* ghost: neutral default action */
  .btn.ghost {
    background: var(--surface-2);
    color: var(--text);
    border-color: var(--line-strong);
  }
  .btn.ghost:hover { border-color: var(--blue); color: var(--blue); filter: none; background: var(--surface-3); }
  .btn.ghost.on {
    border-color: var(--blue);
    color: var(--blue);
    background: var(--blue-soft);
  }
  /* danger: neutral at rest, red only on hover */
  .btn.danger {
    background: var(--surface-2);
    color: var(--text-dim);
    border-color: var(--line-strong);
  }
  .btn.danger:hover {
    background: var(--red-soft);
    color: var(--red);
    border-color: var(--red);
    filter: none;
  }
  .status {
    color: var(--yellow);
    font-size: 13px;
    margin-left: 4px;
  }

  /* Body: terminal stack + optional SFTP drawer, laid out horizontally */
  .body {
    flex: 1;
    min-height: 0;
    display: flex;
  }
  /* Terminal stack */
  .stack {
    flex: 1;
    min-width: 0;
    min-height: 0;
    position: relative;
    background: var(--term-bg);
  }
  .term {
    position: absolute;
    inset: 0;
    padding: 8px 6px 6px 10px;
  }
  .term[hidden] { display: none; }
  .overlay,
  .welcome {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    color: var(--text-mute);
    pointer-events: none;
    background: radial-gradient(circle at 50% 42%, rgba(137, 180, 250, 0.05), transparent 55%);
  }
  .empty-art {
    opacity: 0.5;
    transform: scale(2.6);
    margin-bottom: 14px;
    color: var(--line-strong);
  }
  .empty-art :global(svg) { width: 16px; height: 16px; }
  .empty-title {
    color: var(--text-dim);
    font-size: 15px;
    font-weight: 600;
  }
  .empty-sub {
    color: var(--text-mute);
    font-size: 13px;
  }

  /* Host add / edit form: vertical card over the terminal stack */
  .form-panel {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: flex-start;
    justify-content: center;
    overflow-y: auto;
    padding: 48px 24px;
    background: var(--bg);
  }
  .form-card {
    width: 100%;
    max-width: 400px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .form-card h2 {
    margin: 0 0 2px;
    font-size: 19px;
    font-weight: 700;
    color: var(--text);
    letter-spacing: 0.3px;
  }
  .fld {
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .fld > span {
    font-size: 13px;
    font-weight: 600;
    color: var(--text-dim);
  }
  /* All form controls (text / number / select) share height and font size for alignment */
  .form-card .f {
    width: 100%;
    height: 42px;
    padding: 0 14px;
    font-size: 15px;
  }
  .form-card select.f {
    /* Leave room for the native dropdown arrow so text isn't covered */
    padding-right: 30px;
  }
  .frow { display: flex; gap: 12px; }
  .frow .fld.port { width: 112px; flex: none; }
  .frow .fld.user { flex: 1; min-width: 0; }
  .form-card .chk { font-size: 14px; gap: 7px; margin-top: 2px; }
  .form-card .chk input { width: 15px; height: 15px; }
  .form-actions {
    display: flex;
    gap: 10px;
    margin-top: 8px;
  }
  .form-actions .btn { flex: 1; height: 42px; font-size: 14px; }

  /* Settings: macOS System Settings-style grouped cards, one raised card per group via --card + shadow. */
  .settings-card { gap: 18px; max-width: 540px; }
  .settings-card h2 { margin: 0 0 2px; font-size: 20px; }
  .set-group {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px 18px;
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: 14px;
    box-shadow: var(--shadow-card);
  }
  .set-head { display: flex; flex-direction: column; gap: 3px; }
  .set-title { font-size: 13.5px; font-weight: 650; color: var(--text); letter-spacing: 0.2px; }
  .set-sub { font-size: 12px; color: var(--text-dim); line-height: 1.4; }

  /* Language selection card grid */
  .lang-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  .lang {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 11px 13px;
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 10px;
    cursor: pointer;
    text-align: left;
    transition: border-color 0.12s, background 0.12s, box-shadow 0.12s;
  }
  .lang:hover { border-color: var(--line-strong); background: var(--surface-3); }
  .lang.on {
    border-color: var(--blue);
    background: color-mix(in srgb, var(--blue) 12%, var(--card));
    box-shadow: inset 0 0 0 1px var(--blue);
  }
  .lang:active { transform: translateY(0.5px); }
  .lang-native { font-size: 14px; font-weight: 600; color: var(--text); }
  .lang.on .lang-native { color: var(--blue); }
  .lang-en { font-size: 12px; color: var(--text-mute); }
  .lang-check { position: absolute; top: 10px; right: 11px; color: var(--blue); }

  /* Accent color swatches */
  .swatches { display: flex; gap: 14px; }
  .swatch {
    width: 28px;
    height: 28px;
    border-radius: 50%;
    background: var(--sw);
    border: none;
    padding: 0;
    cursor: pointer;
    box-shadow: 0 0 0 1px var(--line-strong);
    transition: transform 0.12s, box-shadow 0.12s;
  }
  .swatch:hover { transform: scale(1.12); }
  /* Selected: card-colored gap ring + own-color ring, clear even on white cards */
  .swatch.on { box-shadow: 0 0 0 2px var(--card), 0 0 0 4px var(--sw); }

  /* Theme mode segmented control: inset track + accent outline on the active segment */
  .seg {
    display: flex;
    gap: 3px;
    padding: 3px;
    background: var(--surface-3);
    border: 1px solid var(--line);
    border-radius: 10px;
  }
  .seg-btn {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 5px;
    padding: 9px 8px;
    border-radius: 7px;
    border: 1px solid transparent;
    background: transparent;
    color: var(--text-dim);
    font-size: 12.5px;
    cursor: pointer;
    transition: background 0.12s, color 0.12s, box-shadow 0.12s, transform 0.12s;
  }
  .seg-btn:hover:not(.on) { color: var(--text); background: color-mix(in srgb, var(--text) 7%, transparent); }
  .seg-btn.on {
    color: var(--blue);
    background: color-mix(in srgb, var(--blue) 14%, var(--card));
    box-shadow: inset 0 0 0 1.5px var(--blue);
  }
  .seg-btn:active { transform: translateY(0.5px); }

  /* Font-size stepper: inset frame, +/- keys on the card color */
  .stepper {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    width: fit-content;
    padding: 4px;
    background: var(--surface-3);
    border: 1px solid var(--line);
    border-radius: 10px;
  }
  .step {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 30px;
    height: 30px;
    background: var(--card);
    color: var(--text);
    border: 1px solid var(--line);
    border-radius: 7px;
    font-size: 17px;
    line-height: 1;
    cursor: pointer;
    transition: border-color 0.12s, color 0.12s, background 0.12s;
  }
  .step:hover:not(:disabled) { border-color: var(--blue); color: var(--blue); }
  .step:active:not(:disabled) { transform: translateY(0.5px); }
  .step:disabled { opacity: 0.4; cursor: default; }
  .step-val {
    min-width: 64px;
    text-align: center;
    font-size: 13.5px;
    color: var(--text);
    font-variant-numeric: tabular-nums;
  }

  /* Host-key trust prompt (unknown host on first connect) */
  .hk-backdrop {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(0, 0, 0, 0.45);
    -webkit-app-region: no-drag;
  }
  .hk-card {
    width: min(440px, calc(100vw - 48px));
    background: var(--card);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-card);
    padding: 20px;
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .hk-title {
    font-size: 15px;
    font-weight: 600;
    color: var(--text);
  }
  .hk-intro {
    font-size: 13px;
    line-height: 1.5;
    color: var(--text-dim);
  }
  .hk-row {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .hk-label {
    font-size: 11px;
    color: var(--text-mute);
    text-transform: uppercase;
    letter-spacing: 0.03em;
  }
  .hk-val {
    font-size: 13px;
    color: var(--text);
  }
  .hk-fp {
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 12.5px;
    word-break: break-all;
    user-select: text;
  }
  .hk-warn {
    font-size: 12px;
    line-height: 1.5;
    color: var(--yellow);
    background: rgba(249, 226, 175, 0.1);
    border-radius: var(--r-sm);
    padding: 8px 10px;
  }
  .hk-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
  .hk-btn {
    height: 32px;
    padding: 0 16px;
    border-radius: var(--r-sm);
    font-size: 13px;
    font-weight: 600;
    cursor: pointer;
    border: 1px solid var(--line-strong);
    background: var(--surface-2);
    color: var(--text);
    transition: filter 0.12s, border-color 0.12s;
  }
  .hk-btn:hover { border-color: var(--red); color: var(--red); }
  .hk-trust {
    background: var(--blue);
    border-color: transparent;
    color: var(--bg);
  }
  .hk-trust:hover { filter: brightness(1.08); color: var(--bg); }
</style>
