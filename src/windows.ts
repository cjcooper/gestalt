// Closing things in order: ⌘W closes the front dialog before the window, and
// closing the main window closes the file editor windows first, so each can
// save (or ask) before anything goes away.

import { emitTo, listen } from "@tauri-apps/api/event";
import { getAllWebviewWindows } from "@tauri-apps/api/webviewWindow";

/** Editor window labels (src-tauri/src/editor_window.rs). */
export const EDITOR_LABEL_PREFIX = "editor-";
/** Main → editor: close if you can. */
export const CLOSE_EVENT = "editor://close";
/** Editor → main: whether it closed. */
export const closedEvent = (label: string) => `editor://closed/${label}`;
/** Editor → main: run a plan in a folder. */
export const PLAN_EVENT = "editor://plan";
export interface PlanRequest {
  dir: string;
}
/** An editor that doesn't answer (still loading) has nothing to save. */
export const REPLY_TIMEOUT_MS = 5000;

/**
 * ⌘W: close the frontmost dialog, the way Esc would. Returns false if there
 * was none, so the window itself should close.
 */
export function closeFrontDialog(): boolean {
  const dialogs = document.querySelectorAll<HTMLDialogElement>("dialog[open]");
  const front = dialogs[dialogs.length - 1];
  if (!front) return false;
  const cancel = new Event("cancel", { cancelable: true });
  front.dispatchEvent(cancel);
  // Dialogs that don't handle Esc themselves just close.
  if (!cancel.defaultPrevented) front.close();
  return true;
}

/** Ask each file editor window to close, one at a time; false if one stays open. */
export async function closeEditorWindows(): Promise<boolean> {
  const editors = (await getAllWebviewWindows()).filter((w) => w.label.startsWith(EDITOR_LABEL_PREFIX));
  for (const w of editors) {
    let answer!: (closed: boolean) => void;
    const reply = new Promise<boolean>((r) => (answer = r));
    const unlisten = await listen<boolean>(closedEvent(w.label), (e) => answer(e.payload));
    const timer = setTimeout(() => {
      void w.destroy();
      answer(true);
    }, REPLY_TIMEOUT_MS);
    await emitTo(w.label, CLOSE_EVENT);
    const closed = await reply;
    clearTimeout(timer);
    unlisten();
    if (!closed) return false;
  }
  return true;
}
