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

mod ssh;
mod store;

use ssh::AppState;
use std::sync::Mutex;
use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // Native file picker for SFTP upload/download paths.
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState::default())
        // Open local SQLite under app_data_dir for structured data (host list, etc.).
        // Create the dir if missing; panic on failure.
        .setup(|app| {
            let dir = app
                .path()
                .app_data_dir()
                .expect("failed to resolve app_data_dir");
            std::fs::create_dir_all(&dir).expect("failed to create app_data_dir");
            let conn = rusqlite::Connection::open(dir.join("moonshell.db"))
                .expect("failed to open moonshell.db");
            store::init(&conn).expect("failed to init db schema");
            app.manage(store::Db(Mutex::new(conn)));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ssh::ssh_connect,
            ssh::ssh_write,
            ssh::ssh_resize,
            ssh::ssh_disconnect,
            ssh::secret_set,
            ssh::secret_get,
            ssh::secret_delete,
            ssh::sftp_list,
            ssh::sftp_download,
            ssh::sftp_upload,
            ssh::sftp_mkdir,
            ssh::sftp_remove,
            ssh::sftp_rename,
            ssh::ssh_exec,
            store::hosts_list,
            store::hosts_save,
            store::hosts_remove,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
