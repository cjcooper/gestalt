// The file editor window (editor.html, opened by double-clicking a file): a
// real window, so the main window stays usable — edit, switch over, plan.
// The OS draws the close / minimize / maximize buttons; ⌘W closes, ⌘S saves.
//
// With Settings › Save automatically (the default), changes are saved when
// the window closes or loses focus (e.g. you click back to the main window);
// otherwise only on ⌘S / Save, and closing with unsaved changes asks first.
//
// In a module folder (one with a main.tf), Plan saves the file and starts a
// plan in the main window, which comes forward to show it.
//
// A symlinked file (e.g. a shared providers.tf) belongs to the folder it was
// opened from: that's where Plan runs. A banner at the top says edits change
// the real file, for every folder that links to it.

import { invoke } from "@tauri-apps/api/core";
import { emit, emitTo } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { ask, message } from "@tauri-apps/plugin-dialog";
import { createCodeEditor, languageFor } from "./codeEditor";
import { icons } from "./icons";
import { getSettings } from "./settings";
import { CLOSE_EVENT, PLAN_EVENT, closedEvent, type PlanRequest } from "./windows";

interface FileContent {
  relPath: string;
  content: string;
  /** The folder it appears in has a main.tf: offer Plan. */
  isModule: boolean;
  /** Reached through a symlink: the real file (relative to the root if inside it). */
  linkTarget: string | null;
}

/** The backend's stale-write refusal (file_edit.rs). */
const isConflict = (e: unknown) => String(e).includes("changed on disk since you opened it");

/** Fill `root` (the window's body) with an editor for `path`. */
export async function mountFileEditor(root: HTMLElement, path: string, owner = "main"): Promise<void> {
  const win = getCurrentWindow();
  let file: FileContent;
  try {
    file = await invoke<FileContent>("file_read", { path });
  } catch (e) {
    await message(String(e), { title: "Can't open file", kind: "error" });
    await win.destroy();
    return;
  }

  const fileName = file.relPath.split(/[\\/]/).pop()!;
  document.title = file.relPath;
  /** What's on disk, as far as we know: the last content read or saved. */
  let saved = file.content;
  /** A save failed and nothing has been typed since: don't retry on every blur. */
  let failed = false;
  let inflight: Promise<boolean> | null = null;
  let closing = false;

  root.innerHTML = `
    <div class="link-warning" role="note" hidden>${icons.alert}<span>Linked file: this is <code></code>. Changes here affect every place that links to it.</span></div>
    <div class="action-bar">
      <button type="button" class="icon-button" data-action="save" title="Save (⌘S)" aria-label="Save">${icons.save}</button>
      <span class="run-actions" hidden>
        <button type="button" class="with-icon" data-action="plan">${icons.play}Plan</button>
      </span>
      <span class="file-status"></span>
      <span class="spacer"></span>
      <span class="hint"></span>
      <span class="file-lang"></span>
    </div>
    <div class="code-host"></div>
    <div class="notice error" hidden></div>`;
  const q = <T extends HTMLElement>(sel: string) => root.querySelector<T>(sel)!;
  q(".file-lang").textContent = languageFor(fileName)?.label ?? "Plain text";
  if (file.linkTarget !== null) {
    q(".link-warning").hidden = false;
    q(".link-warning code").textContent = file.linkTarget;
  }
  const statusEl = q(".file-status");
  const errorEl = q(".notice");
  const saveBtn = q<HTMLButtonElement>("[data-action=save]");
  const hint = q(".hint");

  const editor = createCodeEditor(q(".code-host"), {
    fileName,
    doc: file.content,
    onChange: () => {
      failed = false;
      errorEl.hidden = true;
      render();
    },
    onBlur: () => autoSave(),
  });
  const dirty = () => editor.getValue() !== saved;

  function render() {
    const auto = getSettings().autoSave;
    hint.textContent = auto ? "Saves automatically when you close this window or click away." : "⌘S to save.";
    statusEl.className = "file-status" + (inflight ? " saving" : dirty() ? " dirty" : "");
    statusEl.textContent = inflight ? "Saving…" : dirty() ? "Unsaved changes" : "";
    saveBtn.disabled = !dirty() || !!inflight;
  }

  function showError(e: unknown) {
    failed = true;
    errorEl.textContent = String(e);
    errorEl.hidden = false;
  }

  /** Save if there are changes. Resolves whether everything is now saved. */
  async function save(): Promise<boolean> {
    while (inflight) await inflight;
    if (!dirty()) return true;
    const content = editor.getValue();
    const write = async (expected: string | null) => {
      await invoke("file_save", { path, content, expected });
      saved = content;
      failed = false;
      errorEl.hidden = true;
    };
    inflight = (async () => {
      try {
        await write(saved);
        return true;
      } catch (e) {
        if (!isConflict(e)) {
          showError(e);
          return false;
        }
        const overwrite = await ask(`${file.relPath} was changed on disk since you opened it.\n\nOverwrite it with your version?`, {
          title: "File changed on disk",
          kind: "warning",
          okLabel: "Overwrite",
          cancelLabel: "Keep editing",
        });
        if (!overwrite) {
          showError(e);
          return false;
        }
        try {
          await write(null);
          return true;
        } catch (e2) {
          showError(e2);
          return false;
        }
      }
    })();
    render();
    try {
      return await inflight;
    } finally {
      inflight = null;
      render();
    }
  }

  /** Focus left the editor (or the window): save quietly, if that's the setting. */
  function autoSave() {
    if (getSettings().autoSave && !closing && !inflight && !failed && dirty()) void save();
  }

  /** Save or confirm before the window closes. False keeps it open. */
  async function readyToClose(): Promise<boolean> {
    if (closing) return false;
    closing = true;
    try {
      if (!dirty()) return true;
      if (getSettings().autoSave) {
        return (
          (await save()) ||
          (await ask("Your changes couldn't be saved. Close the editor and discard them?", {
            title: fileName,
            kind: "warning",
            okLabel: "Discard",
            cancelLabel: "Keep editing",
          }))
        );
      }
      return await ask("Discard your unsaved changes?", { title: fileName, kind: "warning", okLabel: "Discard", cancelLabel: "Keep editing" });
    } finally {
      closing = false;
    }
  }

  // The window's close button, ⌘W (app menu) or win.close().
  await win.onCloseRequested(async (ev) => {
    if (!(await readyToClose())) ev.preventDefault();
  });
  // Its workspace window is closing and asks its editors first.
  await win.listen(CLOSE_EVENT, async () => {
    const ok = await readyToClose();
    await emit(closedEvent(win.label), ok);
    if (ok) await win.destroy();
  });

  saveBtn.onclick = () => void save();

  // Plan the folder the file appears in (where it's linked from, for a
  // link), in the workspace window it was opened from.
  const dir = path.slice(0, Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\")));
  const relDir = file.relPath.split(/[\\/]/).slice(0, -1).join("/") || "the root folder";
  const planBtn = q<HTMLButtonElement>("[data-action=plan]");
  planBtn.title = `Save, then plan ${relDir}`;
  q(".run-actions").hidden = !file.isModule;
  planBtn.onclick = async () => {
    if (await save()) await emitTo<PlanRequest>(owner, PLAN_EVENT, { dir });
  };

  // ⌘S saves wherever the focus is; ⌘W closes if the menu didn't already.
  document.addEventListener("keydown", (ev) => {
    if (!(ev.metaKey || ev.ctrlKey) || ev.altKey || ev.shiftKey) return;
    const key = ev.key.toLowerCase();
    if (key === "s") {
      ev.preventDefault();
      void save();
    } else if (key === "w") {
      ev.preventDefault();
      void win.close();
    }
  });
  // Switching to another window or app counts as clicking away.
  window.addEventListener("blur", autoSave);

  render();
  editor.focus();
}
