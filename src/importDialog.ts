// The "Import…" form: asks for everything `tofu import` accepts. Blank fields
// are omitted from the command; filled ones are passed through.

import { invoke } from "@tauri-apps/api/core";

interface VariableInfo {
  name: string;
  typeExpr: string | null;
  description: string | null;
  default: string | null;
  hasDefault: boolean;
  sensitive: boolean;
  providedBy: string | null;
}

interface ResourceInfo {
  address: string;
  repeated: "count" | "for_each" | null;
}

interface ModuleInfo {
  variables: VariableInfo[];
  resources: ResourceInfo[];
  varFiles: string[];
  parseErrors: string[];
}

export interface ImportRequest {
  address: string;
  id: string;
  vars: { name: string; value: string }[];
  varFile: string | null;
  config: string | null;
  lock: boolean | null;
  lockTimeout: string | null;
  parallelism: number | null;
  ignoreRemoteVersion: boolean;
}

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

function field(label: string, input: HTMLElement, hint?: string | Node, required = false): HTMLElement {
  const id = `f-${Math.random().toString(36).slice(2)}`;
  input.id = id;
  const lab = h("label", { htmlFor: id }, label);
  if (required) lab.append(h("span", { class: "req" }, " *"));
  return h("div", { class: "field" }, lab, input, ...(hint ? [h("div", { class: "hint" }, hint)] : []));
}

function variableRow(v: VariableInfo): { row: HTMLElement; input: HTMLInputElement; unresolved: boolean } {
  const unresolved = !v.hasDefault && !v.providedBy;
  let placeholder: string;
  if (v.providedBy) placeholder = `from ${v.providedBy}`;
  else if (v.hasDefault) placeholder = v.sensitive ? "default (sensitive)" : `default: ${v.default}`;
  else placeholder = "no value set — required";

  const input = h("input", {
    type: v.sensitive ? "password" : "text",
    placeholder,
    autocomplete: "off",
    spellcheck: false,
  });
  if (unresolved) input.classList.add("unresolved");

  const name = h("div", { class: "var-name" }, h("code", {}, v.name));
  if (v.typeExpr) name.append(h("span", { class: "muted" }, ` ${v.typeExpr}`));
  if (v.sensitive) name.append(h("span", { class: "tag" }, "sensitive"));
  const row = h("div", { class: "var-row" }, name, input);
  if (v.description) row.append(h("div", { class: "hint var-desc" }, v.description));
  return { row, input, unresolved };
}

/** Show the import form for `dir`. Resolves with the request, or null if cancelled. */
export async function openImportDialog(dir: string, shownDir: string): Promise<ImportRequest | null> {
  const dialog = h("dialog", { class: "modal import-dialog" });
  const form = h("form", { method: "dialog" });
  const body = h("div", { class: "modal-body" }, h("p", { class: "muted" }, "Reading module…"));
  const submit = h("button", { type: "submit", class: "primary", disabled: true }, "Import");
  const cancel = h("button", { type: "button" }, "Cancel");

  form.append(
    h("header", { class: "modal-header" }, h("h2", {}, "Import into "), h("code", {}, shownDir)),
    body,
    h("footer", { class: "modal-footer" }, h("span", { class: "spacer" }), cancel, submit),
  );
  dialog.append(form);
  document.body.append(dialog);
  dialog.showModal();

  let info: ModuleInfo;
  try {
    info = await invoke<ModuleInfo>("module_info", { dir });
  } catch (e) {
    info = { variables: [], resources: [], varFiles: [], parseErrors: [String(e)] };
  }

  // ---- Resource ----
  const listId = `addr-${Math.random().toString(36).slice(2)}`;
  const address = h("input", {
    placeholder: "e.g. aws_s3_bucket.logs",
    autocomplete: "off",
    spellcheck: false,
  });
  address.setAttribute("list", listId);
  const datalist = h("datalist", { id: listId }, ...info.resources.map((r) => h("option", { value: r.address })));
  const addressHint = h("span", {}, `${info.resources.length} resources found in this module.`);
  const id = h("input", { placeholder: "provider ID of the existing object", autocomplete: "off", spellcheck: false });

  const updateAddressHint = () => {
    const r = info.resources.find((r) => r.address === address.value.trim());
    addressHint.textContent = r?.repeated
      ? `Uses ${r.repeated}: add ${r.repeated === "count" ? "an index, e.g. [0]" : 'a key, e.g. ["name"]'}.`
      : `${info.resources.length} resources found in this module. Module resources: module.<name>.<type>.<name>.`;
  };
  updateAddressHint();

  // ---- Variables ----
  const varInputs = info.variables.map((v) => ({ v, ...variableRow(v) }));
  const unresolvedCount = varInputs.filter((x) => x.unresolved).length;

  // ---- Options ----
  const varFilesId = `vf-${Math.random().toString(36).slice(2)}`;
  const varFile = h("input", { placeholder: "none", autocomplete: "off", spellcheck: false });
  varFile.setAttribute("list", varFilesId);
  const varFileList = h("datalist", { id: varFilesId }, ...info.varFiles.map((f) => h("option", { value: f })));
  const config = h("input", { placeholder: "this folder", autocomplete: "off", spellcheck: false });
  const lock = h(
    "select",
    {},
    h("option", { value: "" }, "default (lock state)"),
    h("option", { value: "true" }, "true"),
    h("option", { value: "false" }, "false — don't lock state"),
  );
  const lockTimeout = h("input", { placeholder: "0s", autocomplete: "off" });
  const parallelism = h("input", { type: "number", min: "1", placeholder: "10" });
  const ignoreRemote = h("input", { type: "checkbox" });

  const sections: HTMLElement[] = [];
  if (info.parseErrors.length) {
    sections.push(
      h("div", { class: "notice error" }, "Some files couldn't be read; suggestions may be incomplete:", h("br"), info.parseErrors.join("\n")),
    );
  }
  sections.push(
    h(
      "section",
      {},
      h("h3", {}, "Resource"),
      field("Address", address, addressHint, true),
      datalist,
      field("ID", id, "The ID tofu's provider uses for this object, e.g. a bucket name or zone ID.", true),
    ),
  );
  if (varInputs.length) {
    sections.push(
      h(
        "section",
        {},
        h("h3", {}, "Variables"),
        h(
          "p",
          { class: "hint" },
          "Leave blank to use the value shown. Anything you enter is passed as -var and overrides it.",
          ...(unresolvedCount ? [h("br"), h("span", { class: "error" }, `${unresolvedCount} variable(s) have no value and must be filled.`)] : []),
        ),
        ...varInputs.map((x) => x.row),
      ),
    );
  }
  sections.push(
    h(
      "section",
      {},
      h("h3", {}, "Options"),
      h(
        "div",
        { class: "grid" },
        field("-var-file", varFile, "Extra tfvars file. *.auto.tfvars are loaded anyway."),
        varFileList,
        field("-config", config, "Folder with the configuration to use."),
        field("-lock", lock),
        field("-lock-timeout", lockTimeout, "How long to wait for the state lock."),
        field("-parallelism", parallelism),
        field("-ignore-remote-version", h("label", { class: "check" }, ignoreRemote, " Ignore remote backend version check")),
      ),
    ),
  );
  body.replaceChildren(...sections);
  address.focus();

  const validate = () => {
    submit.disabled = !address.value.trim() || !id.value.trim();
  };
  address.addEventListener("input", () => {
    validate();
    updateAddressHint();
  });
  id.addEventListener("input", validate);
  validate();

  const blank = (s: string) => (s.trim() ? s.trim() : null);

  return new Promise((resolve) => {
    let result: ImportRequest | null = null;
    cancel.onclick = () => dialog.close();
    form.onsubmit = (ev) => {
      if (submit.disabled) {
        ev.preventDefault();
        return;
      }
      const p = parseInt(parallelism.value, 10);
      result = {
        address: address.value.trim(),
        id: id.value.trim(),
        vars: varInputs.filter((x) => x.input.value !== "").map((x) => ({ name: x.v.name, value: x.input.value })),
        varFile: blank(varFile.value),
        config: blank(config.value),
        lock: lock.value === "" ? null : lock.value === "true",
        lockTimeout: blank(lockTimeout.value),
        parallelism: Number.isFinite(p) && p > 0 ? p : null,
        ignoreRemoteVersion: ignoreRemote.checked,
      };
    };
    dialog.addEventListener("close", () => {
      dialog.remove();
      resolve(result);
    });
  });
}
