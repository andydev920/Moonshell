# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`Moonshell` is a lightweight macOS SSH client. The whole point is to be ~an order of magnitude leaner than FinalShell: it uses the **system WebView** (via Tauri) instead of a bundled Chromium, targeting tens of MB of RAM. Keep that constraint in mind — don't pull in heavy frontend deps or anything that would bloat the binary/memory.

This folder is its own project; it is **not** a git repo on its own and lives inside the `moonya/` workspace. See `README.md` for the roadmap. (The folder is still named `luo/` on disk and the crate/package was originally `luo` — the product was renamed to Moonshell.)

## Commands

```bash
pnpm install
pnpm tauri dev      # run the app — first run compiles the whole Rust binary, slow
pnpm tauri build    # produce .app / .dmg
pnpm check          # svelte-kit sync + svelte-check (TS/Svelte typecheck) — run before considering frontend work done
```

There is no test suite yet. `pnpm dev` runs only the Vite frontend (no Rust shell) and is rarely what you want — use `pnpm tauri dev`.

Rust: edit under `src-tauri/`, then `pnpm tauri dev` recompiles. For a faster Rust-only check, `cd src-tauri && cargo check`.

## Architecture

Data flows in one loop across three layers:

```
xterm.js (web terminal)  ⇄  Tauri IPC  ⇄  Rust session manager  ⇄  russh (SSH/PTY)  ⇄  server
```

**Backend — `src-tauri/src/ssh.rs` is the core.** Everything SSH lives here.
- Each session = one PTY+shell channel on a russh connection, driven by its own async task spawned in `ssh_connect`.
- That task runs a `tokio::select!` loop doing two things at once: (1) `channel.wait()` reads server output → `app.emit("ssh-output", …)`; (2) `rx.recv()` pulls front-end commands off an `mpsc::UnboundedSender` and writes them back to the channel.
- Global state is `AppState { sessions: Arc<Mutex<HashMap<id, mpsc::Sender>>> }`, registered via Tauri `.manage()` in `lib.rs`. The task removes itself from the map on exit and emits `ssh-closed`.
- Tauri commands are the only IPC surface, grouped by area: SSH (`ssh_connect` / `ssh_write` / `ssh_resize` / `ssh_disconnect`), Keychain (`secret_set` / `secret_get` / `secret_delete`), SFTP (`sftp_*`), and persistence (`hosts_list` / `hosts_save` / `hosts_remove`). New commands must be added to BOTH the `#[tauri::command]` fn and the `generate_handler!` list in `src-tauri/src/lib.rs`.

**Persistence — `src-tauri/src/store.rs`.** Durable structured data lives in a local **SQLite** DB (`rusqlite`, `bundled` feature → SQLite statically linked, ~+1MB, negligible runtime memory). The file is `moonshell.db` under `app_data_dir` (`~/Library/Application Support/com.youfuu.moonshell/`). This is deliberate: localStorage lives in the WebView cache store and can be wiped by clearing cache / WebView resets, whereas `app_data_dir` survives until the app's support folder is deleted ("don't delete the app → data stays"). The connection is opened once in `lib.rs` `.setup()` and managed as `store::Db(Mutex<Connection>)`. Currently stores the host list (`hosts` table); command history / known_hosts / port forwards / transfer logs are the natural next tenants. `src/lib/hosts.svelte.ts` is the frontend store: it reads/writes via the `hosts_*` IPC commands and runs a one-time migration of any legacy `luo.hosts` localStorage payload into SQLite (+ Keychain) on first load, then deletes the old key. **Passwords never enter SQLite** — plaintext stays in the macOS Keychain (`secret_*`), the DB only keeps a `savePassword` flag.

**Frontend — `src/routes/+page.svelte` is the whole UI** (Svelte 5 runes: `$state`, etc.). It mounts xterm.js + the fit addon, wires `term.onData` → `ssh_write`, listens for `ssh-output`/`ssh-closed`, and debounces `ResizeObserver` → `ssh_resize` (deduped on cols/rows to avoid SIGWINCH storms). Session IDs are `crypto.randomUUID()` minted on the frontend. Event listeners are attached BEFORE `ssh_connect` so the earliest output isn't missed.

**Terminal bytes** cross IPC as JSON number arrays (`Vec<u8>` ⇄ `Uint8Array`). This is the known throughput bottleneck — the planned fix is `tauri::ipc::Channel`.

### SvelteKit specifics
- SPA mode only: `src/routes/+layout.ts` sets `ssr = false`; `adapter-static` with `fallback: index.html`. Frontend builds to `../build`, which Tauri serves (`frontendDist`).
- Dev server is pinned to port 1420 (`strictPort`); `src-tauri/**` is excluded from Vite's watcher.

## Security state (read before touching auth)

The remaining deliberate gap — don't assume it's done:
- `Client::check_server_key` in `ssh.rs` **unconditionally trusts** the server key. known_hosts fingerprint verification is still a planned TODO (step 3). Host management + persistence already landed (SQLite + Keychain, see the Persistence note above), so when you wire up known_hosts, the natural home for fingerprints is a `known_hosts` table in `store.rs`.

Already done: both **password** and **private-key** auth work (`ssh.rs` does `authenticate_password` and `load_secret_key` + `auth_publickey`). Passwords persist in the macOS Keychain (`secret_*`); the DB only keeps a `savePassword` flag. Private-key **passphrases are intentionally not persisted** — prompted per connect.
