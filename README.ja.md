# Moonshell

[English](README.md) · **日本語** · [简体中文](README.zh-CN.md)

1. Tauri で構築した軽量な macOS 向け SSH クライアント —— システムの WebView を利用し、メモリ使用量は 50 MB 未満。  
2. 高速・低メモリな macOS 向け SSH クライアント。Tauri + Rust（russh）でネイティブ WebView を利用し、数百 MB ではなく数十 MB で動作。  
3. スリムな macOS 向け SSH/SFTP クライアント。Tauri + Rust + xterm.js、Chromium をバンドルしません。

## スクリーンショット

| メイン画面 | ホストの追加 |
|---|---|
| ![Moonshell メイン画面](static/readme/ScreenShot_1.png) | ![ホストの追加](static/readme/ScreenShot_2.png) |

## 機能

- **SSH ログイン** — パスワード認証と秘密鍵認証。鍵のパスフレーズは接続のたびに尋ね、保存しません
- **Web ターミナル** — xterm.js によるフル機能ターミナル。双方向エコーとウィンドウの自動リサイズ
- **マルチセッション** — 接続ごとに独立した PTY を持ち、それぞれ独立した非同期タスクで動作
- **手動再接続** — 切断後はワンクリックで再接続、保存済み資格情報を再利用
- **ホスト管理** — サイドバーで追加 / 編集 / 削除、ワンクリック接続
- **ローカル永続化** — ホスト一覧を SQLite に保存。キャッシュ削除 / WebView リセットでも失われません
- **パスワードの安全性** — パスワードは macOS キーチェーンに保存し、平文で DB に書きません
- **known_hosts 検証** — 未知のホストはフィンガープリント確認後に信頼。鍵が変わった場合は接続を拒否
- **SFTP 転送** — リモートディレクトリの閲覧、アップロード / ダウンロード、作成 / 削除 / リネーム
- **モニターパネル** — リモートのメトリクスを定期サンプリング。サーバーにエージェント不要
- **テーマ** — ライト / ダーク / システムに追従、5 種類のアクセントカラー
- **ターミナルのフォントサイズ調整**
- **多言語** — 簡体中文、日本語、English、Français、Deutsch、Español

## 技術スタック

- **シェル / バックエンド**:[Tauri 2](https://tauri.app) + Rust(システム WebView、Chromium 同梱なし)
- **UI**:Svelte 5 + SvelteKit(SPA モード、`ssr = false`)
- **ターミナル**:[xterm.js](https://xtermjs.org) + fit アドオン
- **SSH/PTY**:[russh](https://crates.io/crates/russh) 0.61(純 Rust の非同期実装)

## アーキテクチャ

```
xterm.js (Web ターミナル)  ⇄  Tauri IPC  ⇄  Rust セッションマネージャ  ⇄  russh (SSH/PTY)  ⇄  サーバー
```

- バックエンドの中核:`src-tauri/src/ssh.rs`
  - 各セッション = russh 接続上の 1 つの PTY+shell チャネル。独立した非同期タスクで駆動
  - タスクは `select!` で 2 つを同時に処理:サーバー出力を読む → セッションごとの `tauri::ipc::Channel` で xterm へバイトを直接ストリーム。フロントエンドのコマンドを受け取る → チャネルへ書き戻す
  - コマンド:`ssh_connect` / `ssh_write` / `ssh_resize` / `ssh_disconnect`
- フロントエンド:`src/routes/+page.svelte`(接続フォーム + xterm、`ssh-output` / `ssh-closed` を購読)

## インストール / 実行

前提:Node.js + [pnpm](https://pnpm.io)、Rust ツールチェーン(`rustup`)、Xcode Command Line Tools。

```bash
pnpm install        # フロントエンドの依存を導入

pnpm tauri dev      # アプリを起動(初回は Rust バイナリ全体をコンパイルするため遅い)
pnpm check          # svelte-kit sync + svelte-check 型チェック(フロント作業後はこれを実行)
pnpm tauri build    # ビルド、.app / .dmg を生成
```

> `pnpm tauri dev` は SSH 接続用です。Vite フロントエンドのみ(Rust シェルなし)を起動するコマンドは通常使いません。

## ライセンス

[Apache License 2.0](LICENSE) © 2026 Moonya
