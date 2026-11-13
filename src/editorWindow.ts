// Entry point of a file editor window (editor.html). Which file to edit is
// set by the backend before the page loads (src-tauri/src/editor_window.rs).

import { mountFileEditor } from "./fileEditor";
import { initSystemColors } from "./systemColors";

declare global {
  interface Window {
    /** The file, and the label of the workspace window it was opened from. */
    __GESTALT_EDITOR__?: { path: string; owner?: string };
  }
}

document.documentElement.dataset.platform = navigator.userAgent.includes("Mac") ? "mac" : "other";
initSystemColors();
const target = window.__GESTALT_EDITOR__;
if (target) void mountFileEditor(document.body, target.path, target.owner);
