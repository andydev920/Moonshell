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

// Saved-host local store + reactive state.
// Holds connection metadata (label/host/port/user) + auth kind + savePassword flag.
//
// Backend = local SQLite (Rust store.rs, app_data_dir/moonshell.db); survives WebView cache wipes.
//
// Security: passwords never enter the DB. With savePassword, plaintext goes to macOS Keychain
// (service="moonshell-ssh", account=host id); DB only keeps savePassword:true. Key auth stores
// only keyPath; passphrase is not persisted.

import { invoke } from "@tauri-apps/api/core";

export type AuthKind = "password" | "key";

export type Host = {
  id: string;
  label: string;
  host: string;
  port: number;
  user: string;
  /** Whether the password is stored in Keychain (only for auth==='password'). Default false. */
  savePassword?: boolean;
  /** Auth kind. Default 'password'. */
  auth?: AuthKind;
  /** Private-key path, only for auth==='key'. */
  keyPath?: string;
};

/** Legacy localStorage shape with a plaintext password field. Used only during migration. */
type LegacyHost = Host & { password?: string };

/** Legacy localStorage key, removed after migration. */
const LEGACY_KEY = "luo.hosts";

/** Fills in safe defaults for savePassword / auth on a single host. */
function normalize(h: Host): Host {
  return {
    ...h,
    savePassword: h.savePassword ?? false,
    auth: h.auth ?? "password",
  };
}

class HostStore {
  list = $state<Host[]>([]);
  #loaded = false;

  /**
   * Call once on first mount.
   * 1) One-time migration of any legacy localStorage data into Keychain + SQLite, then drop the key. Idempotent (UPSERT by id).
   * 2) Load the authoritative list from SQLite into reactive state.
   */
  async load() {
    if (this.#loaded) return;
    this.#loaded = true;

    await this.#migrateFromLocalStorage();

    try {
      this.list = await invoke<Host[]>("hosts_list");
    } catch {
      this.list = [];
    }
  }

  /** Migrate legacy localStorage hosts into SQLite + Keychain, then clear the old key. */
  async #migrateFromLocalStorage() {
    let raw: LegacyHost[];
    try {
      const s = localStorage.getItem(LEGACY_KEY);
      if (!s) return;
      raw = JSON.parse(s) as LegacyHost[];
    } catch {
      // Discard unparseable data.
      localStorage.removeItem(LEGACY_KEY);
      return;
    }

    for (const item of raw) {
      const { password, ...rest } = item;
      let host = normalize(rest);

      if (typeof password === "string" && password.length > 0) {
        // Legacy plaintext -> Keychain; mark savePassword on success, degrade gracefully on failure.
        try {
          await invoke("secret_set", { hostId: host.id, password });
          host = { ...host, savePassword: true };
        } catch {
          host = { ...host, savePassword: false };
        }
      }

      try {
        await invoke("hosts_save", { host });
      } catch {
        // Single write failed; keep the localStorage key for retry on next start.
        return;
      }
    }

    localStorage.removeItem(LEGACY_KEY);
  }

  async add(h: Omit<Host, "id">): Promise<Host> {
    const host: Host = normalize({ ...h, id: crypto.randomUUID() });
    await invoke("hosts_save", { host });
    this.list = [...this.list, host];
    return host;
  }

  async update(id: string, patch: Partial<Omit<Host, "id">>) {
    const next = this.list.map((h) =>
      h.id === id ? normalize({ ...h, ...patch }) : h,
    );
    const changed = next.find((h) => h.id === id);
    if (changed) await invoke("hosts_save", { host: changed });
    this.list = next;
  }

  async remove(id: string) {
    await invoke("hosts_remove", { id });
    this.list = this.list.filter((h) => h.id !== id);
  }
}

export const hosts = new HostStore();
