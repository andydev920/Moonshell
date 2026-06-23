# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`Moonshell` is a lightweight macOS SSH client. The whole point is to be ~an order of magnitude leaner than FinalShell: it uses the **system WebView** (via Tauri) instead of a bundled Chromium, targeting tens of MB of RAM. Keep that constraint in mind — don't pull in heavy frontend deps or anything that would bloat the binary/memory.

This folder is its own project; it is **not** a git repo on its own and lives inside the `moonya/` workspace. See `README.md` for the roadmap.

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

**Persistence — `src-tauri/src/store.rs`.** Durable structured data lives in a local **SQLite** DB (`rusqlite`, `bundled` feature → SQLite statically linked, ~+1MB, negligible runtime memory). The file is `moonshell.db` under `app_data_dir` (`~/Library/Application Support/jp.moonya.moonshell/`). This is deliberate: localStorage lives in the WebView cache store and can be wiped by clearing cache / WebView resets, whereas `app_data_dir` survives until the app's support folder is deleted ("don't delete the app → data stays"). The connection is opened once in `lib.rs` `.setup()` and managed as `store::Db(Mutex<Connection>)`. Currently stores the host list (`hosts` table); command history / known_hosts / port forwards / transfer logs are the natural next tenants. `src/lib/hosts.svelte.ts` is the frontend store: it reads/writes via the `hosts_*` IPC commands and runs a one-time migration of any legacy `moonshell.hosts` localStorage payload into SQLite (+ Keychain) on first load, then deletes the old key. **Passwords never enter SQLite** — plaintext stays in the macOS Keychain (`secret_*`), the DB only keeps a `savePassword` flag.

**Frontend — `src/routes/+page.svelte` is the whole UI** (Svelte 5 runes: `$state`, etc.). It mounts xterm.js + the fit addon, wires `term.onData` → `ssh_write`, listens for `ssh-output`/`ssh-closed`, and debounces `ResizeObserver` → `ssh_resize` (deduped on cols/rows to avoid SIGWINCH storms). Session IDs are `crypto.randomUUID()` minted on the frontend. Event listeners are attached BEFORE `ssh_connect` so the earliest output isn't missed.

**Terminal bytes** cross IPC as JSON number arrays (`Vec<u8>` ⇄ `Uint8Array`). This is the known throughput bottleneck — the planned fix is `tauri::ipc::Channel`.

### SvelteKit specifics
- SPA mode only: `src/routes/+layout.ts` sets `ssr = false`; `adapter-static` with `fallback: index.html`. Frontend builds to `../build`, which Tauri serves (`frontendDist`).
- Dev server is pinned to port 1420 (`strictPort`); `src-tauri/**` is excluded from Vite's watcher.

## Security state (read before touching auth)

**Host-key verification (`Client::check_server_key` in `ssh.rs`) is done** and uses `~/.ssh/known_hosts` (russh `check_known_hosts` / `learn_known_hosts`, interoperable with system ssh — deliberately *not* a SQLite table):
- known + matching → allow;
- **unknown host → interactive trust prompt**: the callback mints a request id, emits `ssh-hostkey-prompt` (host/port/algo/SHA256 fingerprint), and **blocks the handshake** awaiting a oneshot. The front-end modal (`+page.svelte`, `hostKeyQueue`) calls `ssh_hostkey_decision(requestId, trust)`, which routes the choice back via `AppState.host_key_prompts`. Trust → `learn_known_hosts` + allow; reject (or dropped sender, e.g. webview reload) → fail closed. **Never silently accept-new.**
- known but **key changed → hard reject** (possible MITM); the user must hand-edit `known_hosts`. This branch is intentionally *not* behind a prompt — keep it that way unless asked.

Already done: both **password** and **private-key** auth work (`ssh.rs` does `authenticate_password` and `load_secret_key` + `auth_publickey`). Passwords persist in the macOS Keychain (`secret_*`); the DB only keeps a `savePassword` flag. Private-key **passphrases are intentionally not persisted** — prompted per connect.

## 签名打包 / 发布

发布形态:**Developer ID + 公证、非沙盒**的 Universal 包(Apple Silicon + Intel)。不上 App Store —— 沙盒会挡住读 `~/.ssh`、私钥文件与 Keychain。

**前置(一次性)**
1. Apple Developer Program 会员(登录 App Store Connect 的付费账号)。
2. **Developer ID Application** 证书:Xcode → Settings → Accounts → Manage Certificates → `+`,或 developer.apple.com 生成,装进 login keychain。核对:`security find-identity -v -p codesigning`。
3. 公证凭证二选一:**App 专用密码**(appleid.apple.com)+ Team ID;或 **App Store Connect API Key**(`.p8` + Issuer ID + Key ID)。

**本机打包**
```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin   # 一次
cp scripts/signing.env.example scripts/signing.env           # 填好(已 gitignore)
./scripts/build-macos.sh
```
产物在 `src-tauri/target/universal-apple-darwin/release/bundle/{dmg,macos}/`。env 齐全时 Tauri 自动签名(hardened runtime)+ 公证 + staple。

**CI 发布**(`.github/workflows/release.yml`)—— 在仓库 Secrets 配 `APPLE_CERTIFICATE`(`.p12` 的 base64)、`APPLE_CERTIFICATE_PASSWORD`、`APPLE_SIGNING_IDENTITY`、`KEYCHAIN_PASSWORD`、`APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID`,然后:
```bash
git tag v0.1.0 && git push origin v0.1.0
```
workflow 会出 Universal 包并起草带 `.dmg`/`.app` 的 Release。改用 API Key 时把后三个换成 `APPLE_API_ISSUER` / `APPLE_API_KEY` / `APPLE_API_KEY_PATH`。

**验证**
```bash
spctl -a -vv Moonshell.app        # 期望:accepted, source=Notarized Developer ID
xcrun stapler validate Moonshell.app
```

### Windows(.exe 安装包,NSIS)

发布形态:**NSIS 安装器(`.exe`),per-user 安装**(`installMode: currentUser`,无需管理员)。密码改存 **Windows 凭据管理器**(`keyring` 的 `windows-native` 后端,按平台条件编译,见 `Cargo.toml` 的 `[target.'cfg(windows)'.dependencies]`)。bundle `targets` 已改为 `"all"`:macOS 出 app+dmg,Windows 出 nsis(+msi)。WebView2 缺失时安装器自动下载(`webviewInstallMode: downloadBootstrapper`)。

跨平台代码已就绪:唯一的 Unix 专属逻辑 `harden_known_hosts_perms` 用 `#[cfg(unix)]` 隔离;known_hosts 走 russh,在 Windows 解析到 `%USERPROFILE%\.ssh\known_hosts`,与系统 ssh 互通。

**本机打包(在 Windows 上)**
```powershell
# 一次性:Rust(MSVC 工具链)+ VS C++ Build Tools + Node 20 + pnpm
pwsh ./scripts/build-windows.ps1
```
产物在 `src-tauri/target/release/bundle/nsis/*.exe`。**代码签名为可选**:本机把证书装进 Windows 证书库后,设 `$env:WINDOWS_CERTIFICATE_THUMBPRINT` 再跑脚本即自动签名;不设则出未签名包(SmartScreen 提示“未知发布者”)。

**CI 发布** —— 同一个 `release.yml`:`create-release` 先建草稿,再用 matrix 在 `macos-latest` / `windows-latest` 分别构建并上传到同一 Release。打 `v*` tag 即触发,macOS 与 Windows 安装包会一起挂到该 Release。Windows 签名接线已就绪但**默认关闭**:仓库 Secrets 配 `WINDOWS_CERTIFICATE`(`.pfx` 的 base64)+ `WINDOWS_CERTIFICATE_PASSWORD` 后,CI 会导入证书、取指纹注入 `tauri.conf.json` 并签名;未配则出未签名包。

## 已知 TODO / 待加固

- **正式签名发布** — macOS 侧配置与 CI 已就绪,待填入 Apple 证书 + 公证凭证跑通一次
- **Windows 代码签名** — 接线已就绪(本机 env + CI Secrets 两条路),待购置 Authenticode 证书后填入跑通一次;未填则安装器未签名(SmartScreen 警告)
