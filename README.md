# Moonshell

轻量 macOS SSH 客户端。目标:比 FinalShell 轻一个数量级(系统 WebView、几十 MB 内存)。

属 **moonya** 系列:moon(月)+ shell(终端外壳)。

## 功能

**连接 / 终端**
- SSH 密码登录与私钥登录(私钥口令每次连接时询问,不落盘)
- 完整 xterm.js 网页终端:双向回显、窗口自适应 resize(去抖去重,避免 SIGWINCH 风暴)
- 多会话:每条连接 = 独立 PTY+shell channel,各跑各的 async 任务

**主机管理 / 持久化**
- 主机列表侧栏,增删改与一键连接
- 本地 SQLite 持久化(存于 `app_data_dir`,清缓存 / WebView 重置不会丢)
- 密码存进 macOS Keychain(SQLite 只留 `savePassword` 标志,明文不入库)
- 首次启动自动迁移旧版 `luo.hosts` localStorage 数据到 SQLite + Keychain

**SFTP 文件传输**
- 远端目录浏览(`sftp_list`)
- 上传 / 下载(走原生文件对话框选本地路径)
- 新建目录、删除、重命名

**服务器监控面板**
- 周期采样远端指标(OS、内核、进程等),无需在服务器装 agent —— 靠 `ssh_exec` 跑命令取数

**界面 / 体验**
- 主题:浅色 / 深色 / 跟随系统,5 种强调色(blue / mauve / green / pink / peach)
- 终端字号可调
- 多语言 i18n:简体中文、日本語、English、Français、Deutsch、Español

## 技术栈

- **壳/后端**:[Tauri 2](https://tauri.app) + Rust(系统 WebView,非打包 Chromium)
- **UI**:Svelte 5 + SvelteKit(SPA 模式,`ssr = false`)
- **终端**:[xterm.js](https://xtermjs.org) + fit 插件
- **SSH/PTY**:[russh](https://crates.io/crates/russh) 0.61(纯 Rust 异步)

## 架构

```
xterm.js (网页终端)  ⇄  Tauri IPC  ⇄  Rust 连接管理器  ⇄  russh (SSH/PTY)  ⇄  服务器
```

- 后端核心:`src-tauri/src/ssh.rs`
  - 每个会话 = 一条 russh 连接上的一个 PTY+shell channel,跑在独立 async 任务里
  - 任务用 `select!` 同时:读服务器输出 → 经每会话 `tauri::ipc::Channel` 直传字节给 xterm;收前端命令 → 写回 channel
  - 命令:`ssh_connect` / `ssh_write` / `ssh_resize` / `ssh_disconnect`
- 前端:`src/routes/+page.svelte`(连接表单 + xterm,监听 `ssh-output`/`ssh-closed`)

## 安装 / 运行

前置:Node.js + [pnpm](https://pnpm.io)、Rust 工具链(`rustup`)、Xcode Command Line Tools。

```bash
pnpm install        # 装前端依赖

pnpm tauri dev      # 跑起 App(首次会编译整个 Rust 二进制,稍久)
pnpm check          # svelte-kit sync + svelte-check 类型检查(改完前端先跑这个)
pnpm tauri build    # 打包,产出 .app / .dmg
```

> `pnpm dev` 只跑 Vite 前端(没有 Rust 壳),一般用不到 —— 要连 SSH 用 `pnpm tauri dev`。

## 进度

- [x] 第 1 步:脚手架(Tauri 2 + Svelte 5 + TS)
- [x] 第 2 步:Hello SSH —— 密码登录、PTY、xterm 双向回显、resize
- [x] 第 3 步:主机列表侧栏 + 持久化(SQLite + Keychain)/ 私钥认证
- [x] 第 4 步:多会话
- [x] 第 5 步:SFTP 文件浏览器
- [x] 第 6 步:监控面板(周期采样命令,无远程 agent)
- [x] 第 7 步:主题 / i18n、known_hosts 校验、终端输出走 Channel、手动断线重连、签名打包脚手架

## 已知 TODO / 待加固

- known_hosts 已做 `accept-new`(未知主机写入放行、密钥变更硬拒绝);可选增强:交互式信任弹窗(`ssh.rs` 现为静默写入)
- 断线重连为手动(`reopen`);自动重连(退避重试)暂不做
- 签名打包配置 + CI 已就绪,缺正式发布跑通一次(需填入 Apple 证书与公证凭证)

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

## License

[Apache License 2.0](LICENSE) © 2026 Moonya
