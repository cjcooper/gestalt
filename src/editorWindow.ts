// Entry point of a file editor window (editor.html). Which file to edit is
// set by the backend before the page loads (src-tauri/src/editor_window.rs).

import { mountFileEditor } from "./fileEditor";
import { initSystemColors } from "./systemColors";

declare global {
  interface Window {
    __GESTALT_EDITOR__?: { path: string };
  }
}

document.documentElement.dataset.platform = navigator.userAgent.includes("Mac") ? "mac" : "other";
initSystemColors();
const target = window.__GESTALT_EDITOR__;
if (target) void mountFileEditor(document.body, target.path);
