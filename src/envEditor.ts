// Editor for a folder's .gestalt.env, with live syntax checking and a view of
// the variables inherited from parent folders.

import { invoke } from "@tauri-apps/api/core";
import { ask } from "@tauri-apps/plugin-dialog";

interface EnvFileContext {
  relPath: string;
  exists: boolean;
  content: string;
  inherited: { name: string; value: string; source: string }[];
  inheritedUnset: { name: string; source: string }[];
  parentErrors: string[];
}

const TEMPLATE = `# Environment for tofu runs in this folder and every folder below it.
# Deeper .gestalt.env files override this one.
#
# KEY=value              plain value (export prefix optional)
# KEY="a $VAR\\tb"         double quotes: $VAR / \${VAR} expansion, \\n \\t escapes
# KEY='literal $x'       single quotes: no expansion
# unset KEY              remove an inherited variable

`;

const h = <K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> & { class?: string } = {},
  ...children: (Node | string)[]
): HTMLElementTagNameMap[K] => {
  const el = document.createElement(tag);
  const { class: cls, ...rest } = props;
  Object.assign(el, rest);
  if (cls) el.className = cls;
  el.append(...children);
  return el;
};

/** Variable names this content sets or unsets (best effort, for highlighting overrides). */
function namesIn(content: string): Set<string> {
  const names = new Set<string>();
  for (const raw of content.split("\n")) {
    const line = raw.trim();
    const unset = /^unset\s+(.+)$/.exec(line);
    if (unset) unset[1].split(/\s+/).forEach((n) => /^[A-Za-z_]\w*$/.test(n) && names.add(n));
    const set = /^(?:export\s+)?([A-Za-z_]\w*)\s*=/.exec(line);
    if (set) names.add(set[1]);
  }
  return names;
}

/**
 * Edit `<dir>/.gestalt.env`. Resolves true if the file was saved or deleted
 * (so the caller can refresh the tree).
 */
export async function openEnvEditor(dir: string): Promise<boolean> {
  let ctx: EnvFileContext;
  try {
    ctx = await invoke<EnvFileContext>("env_file_read", { dir });
  } catch (e) {
    await ask(String(e), { title: "Can't edit .gestalt.env", kind: "error", okLabel: "OK", cancelLabel: "Close" });
    return false;
  }

  const initial = ctx.exists ? ctx.content : TEMPLATE;
  const expected = ctx.exists ? ctx.content : null;

  const dialog = h("dialog", { class: "modal env-editor" });
  const textarea = h("textarea", { value: initial, spellcheck: false, autocomplete: "off", wrap: "off" });
  const status = h("div", { class: "editor-status" });
  const saveError = h("div", { class: "notice error", hidden: true });
  const save = h("button", { type: "button", class: "primary" }, ctx.exists ? "Save" : "Create");
  const cancel = h("button", { type: "button" }, "Cancel");
  const del = h("button", { type: "button", class: "danger", hidden: !ctx.exists }, "Delete file");

  // ---- Inherited panel ----
  const inheritedList = h("div", { class: "inherited-list" });
  const renderInherited = () => {
    const mine = namesIn(textarea.value);
    const rows: HTMLElement[] = ctx.inherited.map((v) => {
      const overridden = mine.has(v.name);
      return h(
        "div",
        { class: "inherited-row" + (overridden ? " overridden" : ""), title: `set in ${v.source}` },
        h("code", { class: "name" }, v.name),
        h("code", { class: "value" }, v.value === "" ? '""' : v.value),
        h("span", { class: "source" }, overridden ? "overridden here" : v.source),
      );
    });
    for (const u of ctx.inheritedUnset) {
      rows.push(
        h("div", { class: "inherited-row unset" }, h("code", { class: "name" }, u.name), h("span", { class: "value muted" }, "unset"), h("span", { class: "source" }, u.source)),
      );
    }
    inheritedList.replaceChildren(
      ...(rows.length ? rows : [h("p", { class: "muted" }, "Nothing inherited — no parent folder has a .gestalt.env.")]),
    );
  };

  const side = h(
    "aside",
    { class: "inherited" },
    h("h3", {}, "Inherited from parent folders"),
    ...(ctx.parentErrors.length ? [h("div", { class: "notice error" }, ctx.parentErrors.join("\n"))] : []),
    inheritedList,
    h("p", { class: "hint" }, "Set a variable here to override it for this folder and below, or `unset NAME` to remove it."),
  );

  dialog.append(
    h(
      "div",
      { class: "modal-frame" },
      h("header", { class: "modal-header" }, h("h2", {}, ctx.exists ? "Edit" : "Create"), h("code", {}, ctx.relPath)),
      h("div", { class: "editor-body" }, h("div", { class: "editor-main" }, textarea, status, saveError), side),
      h("footer", { class: "modal-footer" }, del, h("span", { class: "spacer" }), cancel, save),
    ),
  );
  document.body.append(dialog);
  dialog.showModal();
  textarea.focus();
  if (!ctx.exists) textarea.setSelectionRange(initial.length, initial.length);

  // ---- Validation (debounced, using the same parser runs use) ----
  let valid = true;
  let timer: number | undefined;
  const validate = async () => {
    const err = await invoke<string | null>("env_file_validate", { content: textarea.value });
    valid = !err;
    status.className = "editor-status " + (err ? "error" : "ok");
    status.textContent = err ? `✕ ${err}` : "✓ Valid";
    save.disabled = !valid;
  };
  textarea.addEventListener("input", () => {
    saveError.hidden = true;
    window.clearTimeout(timer);
    timer = window.setTimeout(() => void validate(), 200);
    renderInherited();
  });
  // Tab inserts spaces instead of leaving the editor.
  textarea.addEventListener("keydown", (ev) => {
    if (ev.key === "Tab" && !ev.shiftKey) {
      ev.preventDefault();
      textarea.setRangeText("  ", textarea.selectionStart, textarea.selectionEnd, "end");
      textarea.dispatchEvent(new Event("input"));
    }
    if ((ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === "s") {
      ev.preventDefault();
      save.click();
    }
  });
  void validate();
  renderInherited();

  return new Promise((resolve) => {
    let changed = false;
    const close = () => {
      dialog.close();
      dialog.remove();
      resolve(changed);
    };
    const dirty = () => textarea.value !== initial;

    const confirmDiscard = async () =>
      !dirty() ||
      (await ask("Discard your unsaved changes?", { title: ".gestalt.env", kind: "warning", okLabel: "Discard", cancelLabel: "Keep editing" }));

    cancel.onclick = async () => {
      if (await confirmDiscard()) close();
    };
    // Esc: same as Cancel (don't let the dialog close itself).
    dialog.addEventListener("cancel", (ev) => {
      ev.preventDefault();
      cancel.click();
    });

    save.onclick = async () => {
      if (!valid) return;
      try {
        await invoke("env_file_save", { dir, content: textarea.value, expected });
        changed = true;
        close();
      } catch (e) {
        saveError.textContent = String(e);
        saveError.hidden = false;
      }
    };

    del.onclick = async () => {
      const ok = await ask(`Delete ${ctx.relPath}?\n\nRuns in this folder and below will no longer get its variables.`, {
        title: "Delete .gestalt.env",
        kind: "warning",
        okLabel: "Delete",
        cancelLabel: "Cancel",
      });
      if (!ok) return;
      try {
        await invoke("env_file_delete", { dir, expected: ctx.content });
        changed = true;
        close();
      } catch (e) {
        saveError.textContent = String(e);
        saveError.hidden = false;
      }
    };
  });
}
