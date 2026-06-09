// Copyright 2026 Moonya
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

// Moonshell SSH/PTY core.
// Each session = one PTY+shell channel on a russh connection, driven by its own async task.
// The task uses select! to do two things at once:
//   1. channel.wait() reads server output -> per-session tauri::ipc::Channel streams bytes to xterm
//   2. recv front-end commands (input data / resize / close) from mpsc -> write back to channel
// Front-end sends commands via ssh_write / ssh_resize / ssh_disconnect.
//
// Connection reuse (single connection, multiple channels):
// Tabs with the same identity (user, host, port) share one authenticated russh connection;
// each tab only channel_open_session for its own channel.
//   - sessions: session id -> command sender of that channel task (routed by session id).
//   - connections: conn key (user@host:port) -> connection state (reuse + refcount).
// Connection lifetime = ConnEntry.members (set of session ids) refcount: a task removes itself
// on exit, and only disconnects + drops the last Arc<Handle> when members hits zero.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use russh::client;
use russh::ChannelMsg;
use serde::{Deserialize, Serialize};
use tauri::ipc::{Channel, Response};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::{mpsc, Mutex, Notify};

/// Fixed Keychain service name; account is the host id.
const KEYCHAIN_SERVICE: &str = "moonshell-ssh";

/// russh client callbacks.
///
/// known_hosts uses ssh `accept-new` semantics (see check_server_key):
///   - unknown host: write the public key to ~/.ssh/known_hosts and allow;
///   - known but key changed: hard reject, handshake fails.
///
/// The callback (&mut self) can't see ConnectArgs, so Client carries the target host/port
/// before connect, plus a shared reject_reason that ssh_connect reads for the precise reason
/// after a failed handshake (KeyChanged handshake errors carry no semantic text).
///
/// TODO: could emit an event + interactive front-end trust/reject prompt instead of accept-new + hard reject.
struct Client {
    host: String,
    port: u16,
    /// Set by the callback on key change / write failure to carry the precise reason.
    reject_reason: Arc<std::sync::Mutex<Option<String>>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        use russh::keys::known_hosts::{check_known_hosts, learn_known_hosts};

        match check_known_hosts(&self.host, self.port, server_public_key) {
            // Known and matching -> allow
            Ok(true) => Ok(true),
            // No entry for this host (unknown) -> accept-new: write and allow
            Ok(false) => {
                if let Err(e) = learn_known_hosts(&self.host, self.port, server_public_key) {
                    if let Ok(mut slot) = self.reject_reason.lock() {
                        *slot = Some(format!("写入 known_hosts 失败: {e}"));
                    }
                    return Err(russh::Error::Keys(e));
                }
                // russh doesn't guarantee perms; set ~/.ssh=0700, known_hosts=0600.
                harden_known_hosts_perms();
                Ok(true)
            }
            // Same-algo entry exists but key mismatches -> key changed: hard reject
            Err(russh::keys::Error::KeyChanged { line }) => {
                if let Ok(mut slot) = self.reject_reason.lock() {
                    *slot = Some(format!(
                        "服务器密钥与 known_hosts 记录不匹配(第 {line} 行),疑似中间人攻击或服务器密钥已变更。\
                         为安全起见已拒绝连接。确认无误后,请手动编辑 ~/.ssh/known_hosts 删除该主机旧行再重连。"
                    ));
                }
                Ok(false)
            }
            // Other read errors (e.g. reading known_hosts failed) -> error
            Err(e) => {
                if let Ok(mut slot) = self.reject_reason.lock() {
                    *slot = Some(format!("读取 known_hosts 失败: {e}"));
                }
                Err(russh::Error::Keys(e))
            }
        }
    }
}

/// Set standard perms (0700 / 0600) on ~/.ssh and known_hosts. Unix only.
fn harden_known_hosts_perms() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Some(home) = std::env::home_dir() {
            let ssh_dir = home.join(".ssh");
            let known = ssh_dir.join("known_hosts");
            let _ = std::fs::set_permissions(&ssh_dir, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::set_permissions(&known, std::fs::Permissions::from_mode(0o600));
        }
    }
}

/// Auth method, passed via ConnectArgs.auth. None falls back to args.password.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum AuthMethod {
    /// Password auth. password is the typed password; empty -> backend reads Keychain by host id.
    Password { password: Option<String> },
    /// Key auth. path is the private-key file; passphrase is optional and not persisted.
    Key {
        path: String,
        passphrase: Option<String>,
    },
}

/// Commands sent to a session task.
enum SessionCmd {
    /// Bytes typed in the terminal
    Data(Vec<u8>),
    /// Terminal resize
    Resize { cols: u32, rows: u32 },
    /// Explicit disconnect
    Close,
}

/// Connection key: reuse key per identity, normalized as "{user}@{host}:{port}".
type ConnKey = String;

/// An established, reusable physical connection.
/// - handle: Arc-wrapped russh handle; channel_open_session needs only &self, so no Mutex<Handle>.
///   The connection closes when the last Arc<Handle> is dropped.
/// - members: set of session ids on this connection; the refcount roster. Recycled at zero.
struct ConnEntry {
    handle: Arc<client::Handle<Client>>,
    members: HashSet<String>,
}

/// Connection state. Dedupes concurrent first-connects: latecomers on the same ConnKey
/// await the first connector's result and reuse it instead of reconnecting + reauthing.
enum ConnState {
    /// Placeholder: first-connect in progress. Latecomers clone the Notify, await it after
    /// releasing the lock, then recheck (Ready -> reuse; gone -> do their own first-connect).
    Connecting(Arc<Notify>),
    /// Established, reusable connection.
    Ready(ConnEntry),
}

/// Global state, registered via Tauri manage().
/// - sessions: session id -> command sender. ssh_write/resize/disconnect route by session id.
/// - connections: conn key -> conn state. Manages physical TCP connection reuse + refcount.
/// - session_conn: session id -> conn key. Lets SFTP resolve a sessionId back to its physical
///   connection (then connections gives the Arc<Handle> for a temp SFTP channel). Lives and
///   dies with sessions: written on ssh_connect, removed when the session task ends.
/// All three use Arc<Mutex<>> to clone handles into session tasks; locks are held only during
/// lookup/insert/update, never across network await (connect/auth/channel_open run lock-free).
#[derive(Default, Clone)]
pub struct AppState {
    sessions: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<SessionCmd>>>>,
    connections: Arc<Mutex<HashMap<ConnKey, ConnState>>>,
    session_conn: Arc<Mutex<HashMap<String, ConnKey>>>,
}

/// Derive the reuse key from connect args. Same (user, host, port) = same connection.
fn conn_key(args: &ConnectArgs) -> ConnKey {
    format!("{}@{}:{}", args.user, args.host, args.port)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectArgs {
    id: String,
    host: String,
    port: u16,
    user: String,
    /// Legacy field: typed-password compat entry. New front-end uses auth=Password{password}.
    #[serde(default)]
    password: String,
    /// Whether this host has a password in Keychain. If typed is empty and this is true, backend reads Keychain by host_id.
    #[serde(default)]
    save_password: bool,
    /// Host id used when saving (Keychain account). Note: id is the per-tab session UUID and
    /// can't read Keychain; passwords are stored by host_id, so use this field to fetch them.
    #[serde(default)]
    host_id: Option<String>,
    /// Auth method. Defaults to password auth (source: typed -> Keychain).
    #[serde(default)]
    auth: Option<AuthMethod>,
    cols: u32,
    rows: u32,
}

// High-frequency terminal output: a per-session tauri::ipc::Channel<Response> streams raw bytes
// (JS receives ArrayBuffer) instead of app.emit("ssh-output") -> serde JSON number arrays,
// skipping JSON encode/decode on both ends. See ssh_connect's on_output and output_ch.send(...).
// ssh-closed is low-frequency and stays an event (ClosedPayload / Emitter).

#[derive(Serialize, Clone)]
struct ClosedPayload {
    id: String,
}

/// Get a Keychain entry handle.
fn keychain_entry(host_id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(KEYCHAIN_SERVICE, host_id)
        .map_err(|e| format!("访问钥匙串失败: {e}"))
}

/// Resolve the password: typed first; else if save_password, read Keychain by host id.
/// Plaintext avoids JS: on the save_password path the password is fetched entirely in the backend.
fn resolve_password(args: &ConnectArgs, typed: Option<String>) -> Result<String, String> {
    if let Some(p) = typed {
        if !p.is_empty() {
            return Ok(p);
        }
    }
    if !args.password.is_empty() {
        return Ok(args.password.clone());
    }
    if args.save_password {
        // Keychain stores by host_id (see secret_set); args.id is the session UUID, unusable.
        let account = args
            .host_id
            .as_deref()
            .ok_or("缺少主机 id,无法从钥匙串取密码,请手动输入")?;
        match keychain_entry(account)?.get_password() {
            Ok(p) => return Ok(p),
            Err(keyring::Error::NoEntry) => {
                return Err("未在钥匙串中找到该主机的密码,请手动输入".into())
            }
            Err(e) => return Err(format!("读取钥匙串密码失败: {e}")),
        }
    }
    Ok(String::new())
}

/// Full first-connect: client::connect (incl. known_hosts accept-new) + auth per AuthMethod.
/// Returns the authenticated owned handle. All network await runs here, outside the connections lock.
async fn establish_connection(args: &ConnectArgs) -> Result<client::Handle<Client>, String> {
    // Dead-connection detection: keepalive every 20s, declare dead and disconnect after 3 misses
    // (~60s). Otherwise on a pulled cable / killed sshd, channel.wait() and SFTP's tokio::io::copy
    // hang forever, pinning the Arc<Handle> so the connection is never reclaimed.
    let config = Arc::new(client::Config {
        keepalive_interval: Some(std::time::Duration::from_secs(20)),
        keepalive_max: 3,
        ..Default::default()
    });

    // Reject reason written back by the callback (KeyChanged etc.); used to build the error.
    let reject_reason: Arc<std::sync::Mutex<Option<String>>> = Arc::new(std::sync::Mutex::new(None));
    let handler = Client {
        host: args.host.clone(),
        port: args.port,
        reject_reason: reject_reason.clone(),
    };

    let mut handle = client::connect(config, (args.host.as_str(), args.port), handler)
        .await
        .map_err(|e| {
            // Prefer the callback's precise reason (key change etc.), else a generic failure.
            if let Some(reason) = reject_reason.lock().ok().and_then(|r| r.clone()) {
                reason
            } else {
                format!("连接失败: {e}")
            }
        })?;

    // Auth: dispatch by AuthMethod, default to password.
    match args.auth {
        Some(AuthMethod::Key {
            ref path,
            ref passphrase,
        }) => {
            let key = russh::keys::load_secret_key(path, passphrase.as_deref())
                .map_err(|e| format!("读取私钥失败: {e}(检查路径或 passphrase 是否正确)"))?;
            let kha = russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None);
            let auth = handle
                .authenticate_publickey(&args.user, kha)
                .await
                .map_err(|e| format!("密钥认证出错: {e}"))?;
            if !auth.success() {
                return Err("密钥认证失败:私钥不被服务器接受或用户名错误".into());
            }
        }
        Some(AuthMethod::Password { ref password }) => {
            let pw = resolve_password(args, password.clone())?;
            let auth = handle
                .authenticate_password(&args.user, &pw)
                .await
                .map_err(|e| format!("认证出错: {e}"))?;
            if !auth.success() {
                return Err("认证失败:用户名或密码错误".into());
            }
        }
        // Legacy front-end compat: no auth field -> password auth (source: args.password -> Keychain).
        None => {
            let pw = resolve_password(args, None)?;
            let auth = handle
                .authenticate_password(&args.user, &pw)
                .await
                .map_err(|e| format!("认证出错: {e}"))?;
            if !auth.success() {
                return Err("认证失败:用户名或密码错误".into());
            }
        }
    }

    Ok(handle)
}

/// Acquire an Arc<Handle> for a usable connection: reuse an existing one, or (deduped) first-connect.
/// On success the returned Arc's ConnEntry already includes args.id in members.
/// Caller then opens a channel; if that fails, it must roll back members (see ssh_connect).
async fn acquire_connection(
    state: &AppState,
    args: &ConnectArgs,
) -> Result<Arc<client::Handle<Client>>, String> {
    let key = conn_key(args);

    loop {
        // Hold the lock only for the lookup, never across await.
        let mut conns = state.connections.lock().await;
        match conns.get(&key) {
            // Reusable connection exists: probe it.
            Some(ConnState::Ready(entry)) => {
                if entry.handle.is_closed() {
                    // Dead: drop the old entry and insert a placeholder in place, we rebuild it (same lock, no race window).
                    conns.remove(&key);
                    conns.insert(key.clone(), ConnState::Connecting(Arc::new(Notify::new())));
                    // Falls through to the first-connect path after match.
                } else {
                    // Alive: add self to members and reuse (skip connect/auth/known_hosts).
                    let handle = entry.handle.clone();
                    if let Some(ConnState::Ready(entry)) = conns.get_mut(&key) {
                        entry.members.insert(args.id.clone());
                    }
                    return Ok(handle);
                }
            }
            // First-connect in progress: register the waiter (Notified::enable) while still holding
            // the lock, then release and await. notify_waiters() only wakes waiters registered at
            // call time, so enabling before dropping the lock avoids a lost-wakeup window.
            Some(ConnState::Connecting(notify)) => {
                let notify = notify.clone();
                let notified = notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                drop(conns);
                notified.await;
                continue;
            }
            // Miss: insert a placeholder, this task does the first-connect.
            None => {
                conns.insert(key.clone(), ConnState::Connecting(Arc::new(Notify::new())));
            }
        }
        drop(conns);

        // Here the placeholder is this task's Connecting, so we run the first-connect.
        // First-connect (connect + auth) runs without holding the lock.
        match establish_connection(args).await {
            Ok(handle) => {
                let handle = Arc::new(handle);
                // Success: store as Ready with self in members, wake all waiters.
                let mut conns = state.connections.lock().await;
                let mut members = HashSet::new();
                members.insert(args.id.clone());
                let entry = ConnEntry {
                    handle: handle.clone(),
                    members,
                };
                if let Some(ConnState::Connecting(notify)) =
                    conns.insert(key.clone(), ConnState::Ready(entry))
                {
                    notify.notify_waiters();
                }
                return Ok(handle);
            }
            Err(e) => {
                // Failure: remove the placeholder and wake waiters (so they retry first-connect).
                let mut conns = state.connections.lock().await;
                if let Some(ConnState::Connecting(notify)) = conns.remove(&key) {
                    notify.notify_waiters();
                }
                return Err(e);
            }
        }
    }
}

/// Remove a session id from a connection's members; if members goes empty, recycle the ConnEntry
/// and return the handle to disconnect outside the lock (Some = caller owns the disconnect).
/// Empty-check + remove happen under the lock so only one caller gets the handle (no double disconnect).
async fn release_member(state: &AppState, key: &ConnKey, id: &str) -> Option<Arc<client::Handle<Client>>> {
    let mut conns = state.connections.lock().await;
    if let Some(ConnState::Ready(entry)) = conns.get_mut(key) {
        entry.members.remove(id);
        if entry.members.is_empty() {
            // Members at zero: take the whole entry (Arc included) for the caller to disconnect outside the lock.
            if let Some(ConnState::Ready(entry)) = conns.remove(key) {
                return Some(entry.handle);
            }
        }
    }
    None
}

/// Establish an SSH connection, open PTY+shell, spawn a background task to pump data. Returns
/// immediately; output is pushed via the per-session Channel (close stays an event). Same-identity
/// tabs reuse one connection (see file header).
///
/// on_output: a Channel<ArrayBuffer> the front-end creates per session and passes via invoke
/// (Tauri injects it). The session task owns it and uses on_output.send(Response::new(bytes)) to
/// stream binary to this tab's terminal, per-session by construction.
#[tauri::command]
pub async fn ssh_connect(
    app: AppHandle,
    state: State<'_, AppState>,
    args: ConnectArgs,
    on_output: Channel<Response>,
) -> Result<(), String> {
    let key = conn_key(&args);

    // Acquire the connection (reuse or deduped first-connect); members now includes this session id.
    let handle = acquire_connection(&state, &args).await?;

    // Open this tab's own channel on the shared handle (&self, no serialization needed).
    let mut channel = match handle.channel_open_session().await {
        Ok(ch) => ch,
        Err(e) => {
            // channel open failed: roll back the just-added member (recycle connection if empty), then error.
            if let Some(h) = release_member(&state, &key, &args.id).await {
                let _ = h
                    .disconnect(russh::Disconnect::ByApplication, "", "")
                    .await;
            }
            return Err(format!("打开会话失败: {e}"));
        }
    };

    if let Err(e) = channel
        .request_pty(false, "xterm-256color", args.cols, args.rows, 0, 0, &[])
        .await
    {
        let _ = channel.close().await;
        if let Some(h) = release_member(&state, &key, &args.id).await {
            let _ = h.disconnect(russh::Disconnect::ByApplication, "", "").await;
        }
        return Err(format!("申请 PTY 失败: {e}"));
    }

    if let Err(e) = channel.request_shell(true).await {
        let _ = channel.close().await;
        if let Some(h) = release_member(&state, &key, &args.id).await {
            let _ = h.disconnect(russh::Disconnect::ByApplication, "", "").await;
        }
        return Err(format!("启动 shell 失败: {e}"));
    }

    let (tx, mut rx) = mpsc::unbounded_channel::<SessionCmd>();
    state.sessions.lock().await.insert(args.id.clone(), tx);
    // Register sessionId -> ConnKey so SFTP can resolve the physical connection. Lives/dies with sessions.
    state
        .session_conn
        .lock()
        .await
        .insert(args.id.clone(), key.clone());

    let id = args.id.clone();
    let sessions_handle = state.sessions.clone();
    // The task queries connections for refcount recycling; clone the whole AppState (all Arc inside).
    let conn_state = (*state).clone();
    // The task holds one Arc<Handle> to keep the connection alive; cloned once here.
    let task_handle = handle.clone();
    // Move the output channel into the task (sync command section no longer uses it).
    let output_ch = on_output;

    tauri::async_runtime::spawn(async move {
        // Hold the Arc<Handle> so the connection isn't dropped while this channel lives.
        let _handle = task_handle;
        loop {
            tokio::select! {
                msg = channel.wait() => {
                    match msg {
                        Some(ChannelMsg::Data { ref data }) => {
                            // Raw bytes stream via the Channel (Response::new(Vec<u8>) -> Raw, JS gets ArrayBuffer).
                            // Ignore send failures (webview closed/reconnecting); task exit is driven by Eof/Close.
                            let _ = output_ch.send(Response::new(data.to_vec()));
                        }
                        Some(ChannelMsg::ExtendedData { ref data, .. }) => {
                            let _ = output_ch.send(Response::new(data.to_vec()));
                        }
                        Some(ChannelMsg::Eof)
                        | Some(ChannelMsg::ExitStatus { .. })
                        | Some(ChannelMsg::ExitSignal { .. })
                        | None => {
                            break;
                        }
                        _ => {}
                    }
                }
                cmd = rx.recv() => {
                    match cmd {
                        Some(SessionCmd::Data(bytes)) => {
                            let _ = channel.data(&bytes[..]).await;
                        }
                        Some(SessionCmd::Resize { cols, rows }) => {
                            let _ = channel.window_change(cols, rows, 0, 0).await;
                        }
                        Some(SessionCmd::Close) | None => {
                            // Close only this tab's channel, leaving other tabs on the connection.
                            let _ = channel.eof().await;
                            let _ = channel.close().await;
                            break;
                        }
                    }
                }
            }
        }
        // Cleanup: remove self from sessions.
        sessions_handle.lock().await.remove(&id);
        // Remove the sessionId -> ConnKey mapping so SFTP can't resolve it after the tab closes.
        conn_state.session_conn.lock().await.remove(&id);
        // Remove self from members; if empty, disconnect the returned handle outside the lock.
        if let Some(h) = release_member(&conn_state, &key, &id).await {
            let _ = h.disconnect(russh::Disconnect::ByApplication, "", "").await;
        }
        // This task's Arc<Handle> (_handle) drops at scope end; when members is zero it's the last
        // reference and the connection closes. Otherwise other channel tasks keep it alive.
        let _ = app.emit("ssh-closed", ClosedPayload { id });
    });

    Ok(())
}

#[tauri::command]
pub async fn ssh_write(
    state: State<'_, AppState>,
    id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    let sessions = state.sessions.lock().await;
    if let Some(tx) = sessions.get(&id) {
        tx.send(SessionCmd::Data(data))
            .map_err(|_| "会话已关闭".to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn ssh_resize(
    state: State<'_, AppState>,
    id: String,
    cols: u32,
    rows: u32,
) -> Result<(), String> {
    let sessions = state.sessions.lock().await;
    if let Some(tx) = sessions.get(&id) {
        let _ = tx.send(SessionCmd::Resize { cols, rows });
    }
    Ok(())
}

#[tauri::command]
pub async fn ssh_disconnect(state: State<'_, AppState>, id: String) -> Result<(), String> {
    let tx = state.sessions.lock().await.remove(&id);
    if let Some(tx) = tx {
        let _ = tx.send(SessionCmd::Close);
    }
    Ok(())
}

// ============ Keychain commands ============
// Fixed service "moonshell-ssh", account = host id. Passwords never touch localStorage.

/// Write/overwrite a host's password in the Keychain.
#[tauri::command]
pub async fn secret_set(host_id: String, password: String) -> Result<(), String> {
    keychain_entry(&host_id)?
        .set_password(&password)
        .map_err(|e| format!("保存密码到钥匙串失败: {e}"))
}

/// Read a host's password; returns None if no entry (not an error).
#[tauri::command]
pub async fn secret_get(host_id: String) -> Result<Option<String>, String> {
    match keychain_entry(&host_id)?.get_password() {
        Ok(p) => Ok(Some(p)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("读取钥匙串密码失败: {e}")),
    }
}

/// Delete a host's password; missing entry counts as success (idempotent).
#[tauri::command]
pub async fn secret_delete(host_id: String) -> Result<(), String> {
    match keychain_entry(&host_id)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("删除钥匙串密码失败: {e}")),
    }
}

// ============ SFTP file browsing ============
//   - SFTP reuses the same russh connection (Arc<Handle>); never opens a separate TCP connection.
//   - An SFTP channel is request-scoped temporary: each sftp_* opens a channel + SftpSession,
//     closed on scope drop. Not cached, not counted in ConnEntry.members, so refcounting is
//     untouched — the physical connection's lifetime stays owned by the terminal tab.
//   - Resolution chain: sessionId -> session_conn (sessionId->ConnKey) -> connections (Arc<Handle>).
//   - Large files skip JSON IPC: the backend streams chunks between the local FS and the remote
//     File via tokio::io::copy; the front-end passes only path strings, bytes never enter JS.

use russh_sftp::client::SftpSession;
use tokio::io::AsyncWriteExt;

/// Remote directory entry returned to the front-end (serde camelCase).
/// mtime is unix seconds (None if unavailable); size defaults to 0 if unavailable.
#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SftpEntry {
    name: String,
    is_dir: bool,
    size: u64,
    mtime: Option<u64>,
}

/// Open a temporary SFTP session on an existing connection, keyed by sessionId.
/// Locks only to clone the Arc<Handle> (no await), then opens the channel + request_subsystem("sftp")
/// + SftpSession::new lock-free, upholding the "no network await under the lock" invariant.
/// Returns an error if the tab is closed (sessionId unmapped) or the connection is dead.
async fn open_sftp(state: &AppState, session_id: &str) -> Result<SftpSession, String> {
    const CLOSED: &str = "SFTP 所属的连接已关闭,请重新打开终端";

    // sessionId -> ConnKey
    let key = {
        let map = state.session_conn.lock().await;
        map.get(session_id).cloned().ok_or_else(|| CLOSED.to_string())?
    };

    // ConnKey -> Arc<Handle> (clone the Arc + probe under the lock, no await)
    let handle = {
        let conns = state.connections.lock().await;
        match conns.get(&key) {
            Some(ConnState::Ready(entry)) if !entry.handle.is_closed() => entry.handle.clone(),
            _ => return Err(CLOSED.to_string()),
        }
    };

    // Lock-free: open a temp channel on the shared connection and request the sftp subsystem.
    let channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("打开 SFTP 通道失败: {e}"))?;
    channel
        .request_subsystem(true, "sftp")
        .await
        .map_err(|e| format!("请求 SFTP 子系统失败: {e}"))?;
    SftpSession::new(channel.into_stream())
        .await
        .map_err(|e| format!("初始化 SFTP 会话失败: {e}"))
}

/// List a directory. path is the full remote path (front-end tracks cwd); "." resolves to the login dir.
/// Sorted dirs-first, then by name case-insensitively.
#[tauri::command]
pub async fn sftp_list(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<Vec<SftpEntry>, String> {
    let sftp = open_sftp(&state, &session_id).await?;
    let dir = sftp
        .read_dir(path.as_str())
        .await
        .map_err(|e| format!("读取目录失败: {e}"))?;

    let mut entries: Vec<SftpEntry> = Vec::new();
    for entry in dir {
        let name = entry.file_name();
        // Skip "." / ".." pseudo-entries; going up is handled by front-end breadcrumbs.
        if name == "." || name == ".." {
            continue;
        }
        let meta = entry.metadata();
        entries.push(SftpEntry {
            is_dir: meta.is_dir(),
            size: meta.size.unwrap_or(0),
            mtime: meta.mtime.map(|t| t as u64),
            name,
        });
    }

    // Dirs first, then by name (case-insensitive).
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    });
    Ok(entries)
}

/// Download: remote read-only File -> local tokio::fs::File, streamed in chunks via tokio::io::copy.
#[tauri::command]
pub async fn sftp_download(
    state: State<'_, AppState>,
    session_id: String,
    remote_path: String,
    local_path: String,
) -> Result<(), String> {
    let sftp = open_sftp(&state, &session_id).await?;
    let mut remote = sftp
        .open(remote_path.as_str())
        .await
        .map_err(|e| format!("打开远程文件失败: {e}"))?;
    let mut local = tokio::fs::File::create(&local_path)
        .await
        .map_err(|e| format!("创建本地文件失败: {e}"))?;
    tokio::io::copy(&mut remote, &mut local)
        .await
        .map_err(|e| format!("下载失败: {e}"))?;
    // Ensure the local buffer is flushed to disk.
    local
        .flush()
        .await
        .map_err(|e| format!("写入本地文件失败: {e}"))?;
    Ok(())
}

/// Upload: local tokio::fs::File -> remote created File, streamed in chunks.
/// Must flush + shutdown the remote File after copy, or buffered data may be lost/truncated on drop.
#[tauri::command]
pub async fn sftp_upload(
    state: State<'_, AppState>,
    session_id: String,
    local_path: String,
    remote_path: String,
) -> Result<(), String> {
    let sftp = open_sftp(&state, &session_id).await?;
    let mut local = tokio::fs::File::open(&local_path)
        .await
        .map_err(|e| format!("打开本地文件失败: {e}"))?;
    // sftp.create is truncating (O_TRUNC): once created, the old remote file is cleared. If
    // copy/flush/shutdown then fails (e.g. network loss), a truncated half-file remains. So the
    // three transfer steps run in an inner async, and on failure we best-effort remove the half-file.
    let mut remote = sftp
        .create(remote_path.as_str())
        .await
        .map_err(|e| format!("创建远程文件失败: {e}"))?;
    let result: Result<(), String> = async {
        tokio::io::copy(&mut local, &mut remote)
            .await
            .map_err(|e| format!("上传失败: {e}"))?;
        // flush + shutdown ensure the remote file is fully written.
        remote
            .flush()
            .await
            .map_err(|e| format!("刷新远程文件失败: {e}"))?;
        remote
            .shutdown()
            .await
            .map_err(|e| format!("关闭远程文件失败: {e}"))?;
        Ok(())
    }
    .await;
    if result.is_err() {
        // Drop the write handle before deleting the half-file.
        drop(remote);
        let _ = sftp.remove_file(remote_path.as_str()).await;
    }
    result
}

/// Create a directory.
#[tauri::command]
pub async fn sftp_mkdir(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<(), String> {
    let sftp = open_sftp(&state, &session_id).await?;
    sftp.create_dir(path.as_str())
        .await
        .map_err(|e| format!("新建文件夹失败: {e}"))
}

/// Recursively delete a directory: SFTP rmdir only removes empty dirs, so delete contents first
/// (recursing into subdirs), then the dir itself. Async recursion needs Box::pin to size the future.
fn remove_dir_recursive<'a>(
    sftp: &'a SftpSession,
    path: &'a str,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
    Box::pin(async move {
        let entries = sftp
            .read_dir(path)
            .await
            .map_err(|e| format!("读取目录失败: {e}"))?;
        for entry in entries {
            let name = entry.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let child = format!("{}/{}", path.trim_end_matches('/'), name);
            if entry.metadata().is_dir() {
                remove_dir_recursive(sftp, &child).await?;
            } else {
                sftp.remove_file(child.as_str())
                    .await
                    .map_err(|e| format!("删除文件失败: {e}"))?;
            }
        }
        sftp.remove_dir(path)
            .await
            .map_err(|e| format!("删除目录失败: {e}"))
    })
}

/// Delete. Front-end passes is_dir from the list item: dirs are recursively emptied then removed, files use remove_file.
#[tauri::command]
pub async fn sftp_remove(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
    is_dir: bool,
) -> Result<(), String> {
    let sftp = open_sftp(&state, &session_id).await?;
    if is_dir {
        remove_dir_recursive(&sftp, path.as_str()).await
    } else {
        sftp.remove_file(path.as_str())
            .await
            .map_err(|e| format!("删除文件失败: {e}"))
    }
}

/// Rename/move. old_path and new_path are both full remote paths.
#[tauri::command]
pub async fn sftp_rename(
    state: State<'_, AppState>,
    session_id: String,
    old_path: String,
    new_path: String,
) -> Result<(), String> {
    let sftp = open_sftp(&state, &session_id).await?;
    sftp.rename(old_path.as_str(), new_path.as_str())
        .await
        .map_err(|e| format!("重命名失败: {e}"))
}

// ============ Monitoring: run one command on an existing connection and collect stdout ============
// Same three-step resolution as open_sftp: sessionId -> ConnKey -> Arc<Handle> (clone under the
// lock, no await), then open a temp exec channel lock-free, run the command, return stdout as a
// string; the channel drops at scope end. Request-scoped one-shot exec, not counted in members.
#[tauri::command]
pub async fn ssh_exec(
    state: State<'_, AppState>,
    session_id: String,
    command: String,
) -> Result<String, String> {
    const CLOSED: &str = "监控所属的连接已关闭,请重新打开终端";

    // sessionId -> ConnKey (read and release the lock)
    let key = {
        let map = state.session_conn.lock().await;
        map.get(&session_id).cloned().ok_or_else(|| CLOSED.to_string())?
    };

    // ConnKey -> Arc<Handle> (clone the Arc + probe under the lock, no await)
    let handle = {
        let conns = state.connections.lock().await;
        match conns.get(&key) {
            Some(ConnState::Ready(entry)) if !entry.handle.is_closed() => entry.handle.clone(),
            _ => return Err(CLOSED.to_string()),
        }
    };

    // Lock-free: open a temp channel on the shared connection and run exec.
    // channel.wait() takes &mut self, so channel must be mut.
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| format!("打开监控通道失败: {e}"))?;
    // russh 0.61: exec<A: Into<Vec<u8>>>(want_reply, command); pass String directly.
    channel
        .exec(true, command)
        .await
        .map_err(|e| format!("执行监控命令失败: {e}"))?;

    // Collect stdout until Eof/ExitStatus/channel close. stderr (ExtendedData) is ignored.
    let mut out: Vec<u8> = Vec::new();
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Data { ref data }) => out.extend_from_slice(data),
            Some(ChannelMsg::Eof)
            | Some(ChannelMsg::ExitStatus { .. })
            | Some(ChannelMsg::ExitSignal { .. })
            | None => break,
            _ => {} // ignore ExtendedData (stderr)/WindowAdjusted etc.
        }
    }
    // channel drops here — not counted in members, refcount untouched; connection stays owned by the tab.
    Ok(String::from_utf8_lossy(&out).into_owned())
}
