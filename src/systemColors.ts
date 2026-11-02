// The user's macOS accent and highlight colours (System Settings › Appearance),
// fed into the theme. styles.css reads --os-accent-light / --os-accent-dark /
// --os-highlight-* and falls back to Gestalt's own colours when they're unset
// (other platforms, or the "Multicolor" accent, which means "app's choice").

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface Palette {
  accent: string | null;
  highlight: string;
}

interface SystemColors {
  light: Palette;
  dark: Palette;
}

function apply(colors: SystemColors | null) {
  const style = document.documentElement.style;
  for (const scheme of ["light", "dark"] as const) {
    const p = colors?.[scheme];
    const set = (name: string, value: string | null | undefined) =>
      value ? style.setProperty(name, value) : style.removeProperty(name);
    set(`--os-accent-${scheme}`, p?.accent);
    set(`--os-highlight-${scheme}`, p?.highlight);
  }
}

export function initSystemColors() {
  invoke<SystemColors | null>("system_colors").then(apply, console.error);
  listen<SystemColors | null>("system://colors", ({ payload }) => apply(payload));
}
