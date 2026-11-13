// App settings, kept in localStorage, and the Settings window (⌘,).

export interface Settings {
  /** File editor: save when it closes or loses focus, instead of asking. */
  autoSave: boolean;
}

const KEY = "gestalt.settings";
const DEFAULTS: Settings = { autoSave: true };

export function getSettings(): Settings {
  let stored: unknown = null;
  try {
    stored = JSON.parse(localStorage.getItem(KEY) ?? "null");
  } catch {
    /* storage unavailable or corrupt: defaults */
  }
  const s = { ...DEFAULTS };
  if (stored && typeof stored === "object") {
    const autoSave = (stored as Record<string, unknown>).autoSave;
    if (typeof autoSave === "boolean") s.autoSave = autoSave;
  }
  return s;
}

export function updateSettings(patch: Partial<Settings>) {
  try {
    localStorage.setItem(KEY, JSON.stringify({ ...getSettings(), ...patch }));
  } catch {
    /* storage unavailable */
  }
}

/** Show the Settings window; changes apply as soon as they're made. */
export function openSettings() {
  const existing = document.querySelector<HTMLDialogElement>("dialog.settings");
  if (existing) {
    existing.querySelector<HTMLElement>("input")?.focus();
    return;
  }
  const dialog = document.createElement("dialog");
  dialog.className = "modal settings";
  dialog.innerHTML = `
    <header class="modal-header"><h2>Settings</h2></header>
    <div class="modal-body">
      <section>
        <h3>File editor</h3>
        <label class="check"><input type="checkbox" name="autoSave" /> Save automatically</label>
        <p class="hint">Save changes when you close the editor or click away from it. When off, press ⌘S or click Save, and closing with unsaved changes asks first.</p>
      </section>
    </div>
    <footer class="modal-footer"><span class="spacer"></span><button type="button" class="primary">Done</button></footer>`;
  const autoSave = dialog.querySelector<HTMLInputElement>("input[name=autoSave]")!;
  autoSave.checked = getSettings().autoSave;
  autoSave.onchange = () => updateSettings({ autoSave: autoSave.checked });

  const close = () => {
    dialog.close();
    dialog.remove();
  };
  dialog.querySelector<HTMLButtonElement>("button.primary")!.onclick = close;
  dialog.addEventListener("cancel", (ev) => {
    ev.preventDefault();
    close();
  });
  // Click on the backdrop.
  dialog.addEventListener("click", (ev) => {
    if (ev.target === dialog) close();
  });
  document.body.append(dialog);
  dialog.showModal();
  autoSave.focus();
}
