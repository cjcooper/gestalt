// A workspace window's look, as in Paddock and subduer: on macOS the window
// has no native title bar (the page draws one under the window controls) and
// shows the OS sidebar material, dimmed when inactive, through the side pane,
// toolbar and title bar; the output pane stays opaque. styles.css keys off
// data-vibrancy, set only once the material is on: until then the window is
// transparent with nothing behind it, so the page keeps its own background.

import { invoke } from "@tauri-apps/api/core";
import { Effect, EffectState, getCurrentWindow } from "@tauri-apps/api/window";

export async function initWindowChrome(): Promise<void> {
  const on = await invoke<boolean>("window_vibrancy").catch(() => false);
  if (!on) return;
  try {
    await getCurrentWindow().setEffects({ effects: [Effect.Sidebar], state: EffectState.FollowsWindowActiveState });
  } catch (e) {
    console.error(e);
    return;
  }
  document.documentElement.setAttribute("data-vibrancy", "");
}
