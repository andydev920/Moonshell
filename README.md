# Moonshell

**English** · [日本語](README.ja.md) · [简体中文](README.zh-CN.md)

1. Lightweight macOS SSH client built with Tauri — uses the system WebView,  under 50 MB of RAM.   
2. A fast, memory-light macOS SSH client. Native WebView via Tauri + Rust   (russh), tens of MB instead of hundreds.   
3. Lean macOS SSH/SFTP client. Tauri + Rust + xterm.js, no bundled Chromium.

## Screenshots

| Main window | Add host |
|---|---|
| ![Moonshell main window](static/readme/ScreenShot_1.png) | ![Add host](static/readme/ScreenShot_2.png) |

## Features

- **SSH login** — password and private-key auth; key passphrases are prompted per connection and never stored
- **Web terminal** — full xterm.js terminal with two-way echo and auto-resize
- **Multiple sessions** — each connection gets its own PTY running an independent async task
- **Manual reconnect** — one-click reconnect after a drop, reusing saved credentials
- **Host management** — add / edit / remove in the sidebar, one-click connect
- **Local persistence** — host list stored in SQLite; survives cache clears / WebView resets
- **Password safety** — passwords live in the macOS Keychain, never written to the database in plaintext
- **known_hosts verification** — unknown hosts require fingerprint confirmation before trust; a changed key is rejected
- **SFTP transfer** — browse remote directories, upload / download, create / delete / rename
- **Monitor panel** — periodically samples remote metrics, no agent to install on the server
- **Themes** — light / dark / follow system, 5 accent colors
- **Adjustable terminal font size**
- **Multilingual** — 简体中文, 日本語, English, Français, Deutsch, Español

## Tech stack

- **Shell / backend**: [Tauri 2](https://tauri.app) + Rust (system WebView, not a bundled Chromium)
- **UI**: Svelte 5 + SvelteKit (SPA mode, `ssr = false`)
- **Terminal**: [xterm.js](https://xtermjs.org) + fit addon
- **SSH/PTY**: [russh](https://crates.io/crates/russh) 0.61 (pure-Rust async)

## Architecture

```
xterm.js (web terminal)  ⇄  Tauri IPC  ⇄  Rust session manager  ⇄  russh (SSH/PTY)  ⇄  server
```

- Backend core: `src-tauri/src/ssh.rs`
  - Each session = one PTY+shell channel on a russh connection, driven by its own async task
  - The task uses `select!` to do two things at once: read server output → stream bytes straight to xterm via a per-session `tauri::ipc::Channel`; receive front-end commands → write them back to the channel
  - Commands: `ssh_connect` / `ssh_write` / `ssh_resize` / `ssh_disconnect`
- Frontend: `src/routes/+page.svelte` (connection form + xterm, listens for `ssh-output` / `ssh-closed`)

## Install / run

Prerequisites: Node.js + [pnpm](https://pnpm.io), the Rust toolchain (`rustup`), and Xcode Command Line Tools.

```bash
pnpm install        # install front-end deps

pnpm tauri dev      # run the app (first run compiles the whole Rust binary, slow)
pnpm check          # svelte-kit sync + svelte-check type check (run this before finishing front-end work)
pnpm tauri build    # build, produces .app / .dmg
```

> `pnpm dev` runs only the Vite front-end (no Rust shell) and is rarely what you want — use `pnpm tauri dev` to connect over SSH.

## License

[Apache License 2.0](LICENSE) © 2026 Moonya
