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

// Appearance settings: mode + accent + terminal font size. Svelte 5 runes + localStorage.
//
// - mode: light / dark / auto. Sets `data-theme` on document.documentElement
//   (auto resolved via prefers-color-scheme); CSS switches UI variables from it.
//   `resolved` exposes the real theme for +page.svelte's xterm palette.
// - accent: overrides CSS variables (--blue / --blue-soft / --focus-ring) on
//   document.documentElement; UI accent only, not the terminal ANSI palette.
// - termFont: stores the value only; +page.svelte's $effect writes it to xterm instances.

export type ThemeMode = "light" | "dark" | "auto";
export type Theme = "light" | "dark";

export type Accent = "blue" | "mauve" | "green" | "pink" | "peach";

/** Accent options (Catppuccin Mocha palette). Array order is display order. */
export const ACCENTS: { key: Accent; hex: string; rgb: string }[] = [
  { key: "blue", hex: "#89b4fa", rgb: "137, 180, 250" },
  { key: "mauve", hex: "#cba6f7", rgb: "203, 166, 247" },
  { key: "green", hex: "#a6e3a1", rgb: "166, 227, 161" },
  { key: "pink", hex: "#f5c2e7", rgb: "245, 194, 231" },
  { key: "peach", hex: "#fab387", rgb: "250, 179, 135" },
];

export const TERM_FONT_MIN = 12;
export const TERM_FONT_MAX = 22;
const TERM_FONT_DEF = 15;
const ACCENT_DEF: Accent = "blue";
const MODE_DEF: ThemeMode = "dark";

const ACCENT_KEY = "moonshell.accent";
const FONT_KEY = "moonshell.termFont";
const MODE_KEY = "moonshell.themeMode";

function loadMode(): ThemeMode {
  try {
    const v = localStorage.getItem(MODE_KEY) as ThemeMode | null;
    if (v === "light" || v === "dark" || v === "auto") return v;
  } catch {
    /* ignore */
  }
  return MODE_DEF;
}

/** Whether the system prefers dark; defaults to dark when matchMedia is unavailable. */
function systemDark(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return true;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/** Resolve mode to a real theme: auto follows the system, others pass through. */
function resolveMode(m: ThemeMode): Theme {
  return m === "auto" ? (systemDark() ? "dark" : "light") : m;
}

/** Set data-theme + color-scheme; CSS switches UI variables from it. */
function applyTheme(t: Theme) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  root.setAttribute("data-theme", t);
  root.style.colorScheme = t;
}

function loadAccent(): Accent {
  try {
    const v = localStorage.getItem(ACCENT_KEY) as Accent | null;
    if (v && ACCENTS.some((a) => a.key === v)) return v;
  } catch {
    /* ignore */
  }
  return ACCENT_DEF;
}

function loadFont(): number {
  try {
    const n = Number(localStorage.getItem(FONT_KEY));
    if (Number.isFinite(n) && n >= TERM_FONT_MIN && n <= TERM_FONT_MAX)
      return n;
  } catch {
    /* ignore */
  }
  return TERM_FONT_DEF;
}

/** Write the chosen accent into root CSS variables, overriding :root defaults. */
function applyAccent(a: Accent) {
  if (typeof document === "undefined") return;
  const def = ACCENTS.find((x) => x.key === a) ?? ACCENTS[0];
  const root = document.documentElement;
  root.style.setProperty("--blue", def.hex);
  root.style.setProperty("--blue-soft", `rgba(${def.rgb}, 0.12)`);
  root.style.setProperty("--focus-ring", `0 0 0 2px rgba(${def.rgb}, 0.35)`);
}

class Settings {
  mode = $state<ThemeMode>(loadMode());
  /** Current real theme (auto resolved). +page.svelte switches the xterm palette from it. */
  resolved = $state<Theme>(resolveMode(loadMode()));
  accent = $state<Accent>(loadAccent());
  termFont = $state<number>(loadFont());

  constructor() {
    // Apply saved mode and accent on startup (font size applied later by the component).
    applyTheme(this.resolved);
    applyAccent(this.accent);
    // In auto mode, follow system theme changes live.
    if (typeof window !== "undefined" && window.matchMedia) {
      window
        .matchMedia("(prefers-color-scheme: dark)")
        .addEventListener("change", () => {
          if (this.mode === "auto") {
            this.resolved = systemDark() ? "dark" : "light";
            applyTheme(this.resolved);
          }
        });
    }
  }

  setMode(m: ThemeMode) {
    if (m !== "light" && m !== "dark" && m !== "auto") return;
    this.mode = m;
    this.resolved = resolveMode(m);
    applyTheme(this.resolved);
    try {
      localStorage.setItem(MODE_KEY, m);
    } catch {
      /* ignore */
    }
  }

  setAccent(a: Accent) {
    if (!ACCENTS.some((x) => x.key === a)) return;
    this.accent = a;
    applyAccent(a);
    try {
      localStorage.setItem(ACCENT_KEY, a);
    } catch {
      /* ignore */
    }
  }

  setTermFont(n: number) {
    const v = Math.max(TERM_FONT_MIN, Math.min(TERM_FONT_MAX, Math.round(n)));
    this.termFont = v;
    try {
      localStorage.setItem(FONT_KEY, String(v));
    } catch {
      /* ignore */
    }
  }
}

export const settings = new Settings();
