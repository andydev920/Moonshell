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

// Local persistence: host list in SQLite.
//
// - DB file lives in app_data_dir (~/Library/Application Support/jp.moonya.moonshell/moonshell.db); survives WebView/cache resets.
// - Connection opened once in lib.rs .setup(), managed as Db(Mutex<Connection>); commands lock briefly.
// - Passwords stay in macOS Keychain (ssh.rs secret_*); DB only keeps the savePassword flag.
// - Ordering: list returns by ascending rowid (insertion order); save uses UPSERT (not INSERT OR REPLACE, which rebuilds the row and changes rowid).

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::State;

/// DB handle managed in Tauri global state.
pub struct Db(pub Mutex<Connection>);

/// Mirrors the frontend Host type (camelCase via serde rename).
#[derive(Serialize, Deserialize, Clone)]
pub struct Host {
    pub id: String,
    pub label: String,
    pub host: String,
    pub port: u16,
    pub user: String,
    #[serde(rename = "savePassword", default)]
    pub save_password: bool,
    #[serde(default = "default_auth")]
    pub auth: String,
    #[serde(rename = "keyPath", default)]
    pub key_path: Option<String>,
}

fn default_auth() -> String {
    "password".into()
}

/// Create table (idempotent). Called once at setup.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS hosts (
            id            TEXT PRIMARY KEY,
            label         TEXT NOT NULL,
            host          TEXT NOT NULL,
            port          INTEGER NOT NULL,
            user          TEXT NOT NULL,
            save_password INTEGER NOT NULL DEFAULT 0,
            auth          TEXT NOT NULL DEFAULT 'password',
            key_path      TEXT
        )",
        [],
    )?;
    Ok(())
}

#[tauri::command]
pub fn hosts_list(db: State<'_, Db>) -> Result<Vec<Host>, String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare(
            "SELECT id, label, host, port, user, save_password, auth, key_path
             FROM hosts ORDER BY rowid ASC",
        )
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(Host {
                id: r.get(0)?,
                label: r.get(1)?,
                host: r.get(2)?,
                port: r.get::<_, i64>(3)? as u16,
                user: r.get(4)?,
                save_password: r.get::<_, i64>(5)? != 0,
                auth: r.get(6)?,
                key_path: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for h in rows {
        out.push(h.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Insert or update one row (UPSERT by id, preserving rowid/order).
#[tauri::command]
pub fn hosts_save(db: State<'_, Db>, host: Host) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO hosts (id, label, host, port, user, save_password, auth, key_path)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            label=excluded.label, host=excluded.host, port=excluded.port,
            user=excluded.user, save_password=excluded.save_password,
            auth=excluded.auth, key_path=excluded.key_path",
        rusqlite::params![
            host.id,
            host.label,
            host.host,
            host.port as i64,
            host.user,
            host.save_password as i64,
            host.auth,
            host.key_path,
        ],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn hosts_remove(db: State<'_, Db>, id: String) -> Result<(), String> {
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM hosts WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}
