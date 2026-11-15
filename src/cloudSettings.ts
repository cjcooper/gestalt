// Cloud settings for a folder: which AWS profile, gcloud configuration or
// Azure subscription tofu uses there and in every folder below it. Choices
// come from the providers' own CLI config; settings are kept by Gestalt, not
// in the repo.
//
// In a git repository a folder's settings are shared by every checkout of
// the repo (each worktree too), and one workspace can override them for
// itself only, e.g. a worktree testing a change against a sandbox account.

import { invoke } from "@tauri-apps/api/core";
import { providerLogos, type ProviderId } from "./icons";

/** Shared by every checkout of the repository, or this workspace's override. */
export type Scope = "shared" | "workspace";

/** A setting in effect, the folder it was set on and which settings it's from. */
export interface CloudSetting {
  key: string;
  value: string;
  source: string;
  scope?: Scope;
}

/** A folder's own settings of each scope (cloud_folders). */
export interface OwnSettings {
  shared: Record<string, string>;
  workspace: Record<string, string>;
}

interface Available {
  awsProfiles: { name: string; region: string | null }[];
  gcpConfigurations: { name: string; project: string | null; account: string | null; active: boolean }[];
  azureSubscriptions: { id: string; name: string; tenantId: string; isDefault: boolean }[];
  errors: string[];
}

interface CloudContext {
  dir: string;
  /** Not in a git repository: no workspace overrides. */
  repo: { name: string; top: string } | null;
  own: Record<string, string>;
  workspace: Record<string, string>;
  inherited: CloudSetting[];
  available: Available;
}

interface Choice {
  value: string;
  text: string;
}

interface Field {
  key: string;
  label: string;
  /** A list to pick from, or free text with this placeholder when nothing is inherited. */
  choices?: (a: Available) => Choice[];
  placeholder?: string;
  hint: string;
}

interface Provider {
  id: ProviderId;
  title: string;
  fields: Field[];
}

export const PROVIDERS: Provider[] = [
  {
    id: "aws",
    title: "AWS",
    fields: [
      {
        key: "aws.profile",
        label: "Profile",
        choices: (a) => a.awsProfiles.map((p) => ({ value: p.name, text: p.region ? `${p.name} (${p.region})` : p.name })),
        hint: "From ~/.aws/config and ~/.aws/credentials. Sets AWS_PROFILE and removes static keys (AWS_ACCESS_KEY_ID…) inherited from your shell.",
      },
      {
        key: "aws.region",
        label: "Region",
        placeholder: "the profile's region",
        hint: "Sets AWS_REGION and AWS_DEFAULT_REGION.",
      },
    ],
  },
  {
    id: "gcp",
    title: "Google Cloud",
    fields: [
      {
        key: "gcp.configuration",
        label: "gcloud configuration",
        choices: (a) =>
          a.gcpConfigurations.map((c) => ({
            value: c.name,
            text: [c.name, c.project && `(${c.project})`, c.active && "— active"].filter(Boolean).join(" "),
          })),
        hint: "From ~/.config/gcloud. Sets CLOUDSDK_ACTIVE_CONFIG_NAME.",
      },
      {
        key: "gcp.project",
        label: "Project",
        placeholder: "the configuration's project",
        hint: "Sets GOOGLE_PROJECT and CLOUDSDK_CORE_PROJECT.",
      },
    ],
  },
  {
    id: "azure",
    title: "Azure",
    fields: [
      {
        key: "azure.subscription",
        label: "Subscription",
        choices: (a) =>
          a.azureSubscriptions.map((s) => ({ value: s.id, text: `${s.name} (${s.id})${s.isDefault ? " — default" : ""}` })),
        hint: "From ~/.azure (az login). Sets ARM_SUBSCRIPTION_ID.",
      },
      {
        key: "azure.tenant",
        label: "Tenant",
        placeholder: "the subscription's tenant",
        hint: "Sets ARM_TENANT_ID.",
      },
    ],
  },
];

const FIELDS = PROVIDERS.flatMap((p) => p.fields.map((f) => ({ ...f, provider: p })));

/** Each provider with settings in effect, and its values, e.g. "prd · eu-west-1". */
export function cloudParts(settings: CloudSetting[]): { id: ProviderId; title: string; text: string }[] {
  const value = (key: string) => settings.find((s) => s.key === key)?.value;
  return PROVIDERS.flatMap((p) => {
    const values = p.fields.map((f) => value(f.key)).filter((v): v is string => !!v);
    return values.length ? [{ id: p.id, title: p.title, text: values.join(" · ") }] : [];
  });
}

/** Short summary of settings in effect, e.g. "AWS prd · eu-west-1"; "" if none. */
export function cloudSummary(settings: CloudSetting[]): string {
  return cloudParts(settings)
    .map((p) => `${p.title} ${p.text}`)
    .join(" | ");
}

/** A provider's logo, for the tree, toolbar and dialog. */
export function providerLogo(id: ProviderId, title?: string): HTMLSpanElement {
  const span = h("span", { class: `cloud-logo ${id}` });
  span.innerHTML = providerLogos[id];
  if (title) span.title = title;
  return span;
}

/** Providers a folder's own settings name, for its badges; overrides win. */
export function cloudBadges({ shared, workspace }: OwnSettings): { id: ProviderId; title: string }[] {
  const value = (key: string) =>
    workspace[key] ? `${workspace[key]} (this workspace only)` : shared[key];
  return PROVIDERS.filter((p) => p.fields.some((f) => value(f.key))).map((p) => ({
    id: p.id,
    title: p.fields
      .filter((f) => value(f.key))
      .map((f) => `${p.title} ${f.label.toLowerCase()}: ${value(f.key)}`)
      .join("\n"),
  }));
}

/** Whether any setting in effect is this workspace's override. */
export const hasOverride = (settings: CloudSetting[]) => settings.some((s) => s.scope === "workspace");

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

/**
 * Edit `dir`'s cloud settings. `shown` names the folder for the user and
 * `rel` shortens other folders' paths. Resolves true if settings were saved.
 */
export async function openCloudSettings(dir: string, shown: string, rel: (path: string) => string): Promise<boolean> {
  const dialog = h("dialog", { class: "modal cloud-dialog" });
  const form = h("form", { method: "dialog" });
  const body = h("div", { class: "modal-body" }, h("p", { class: "muted" }, "Reading cloud config…"));
  const error = h("div", { class: "notice error", hidden: true });
  const save = h("button", { type: "submit", class: "primary", disabled: true }, "Save");
  const cancel = h("button", { type: "button" }, "Cancel");
  const clear = h("button", { type: "button", class: "danger", disabled: true }, "Clear all");
  form.append(
    h("header", { class: "modal-header" }, h("h2", {}, "Cloud settings for "), h("code", {}, shown)),
    body,
    h("footer", { class: "modal-footer" }, clear, h("span", { class: "spacer" }), cancel, save),
  );
  dialog.append(form);
  document.body.append(dialog);
  dialog.showModal();

  const closed = new Promise<void>((resolve) =>
    dialog.addEventListener("close", () => {
      dialog.remove();
      resolve();
    }),
  );
  cancel.onclick = () => dialog.close();

  let ctx: CloudContext;
  try {
    ctx = await invoke<CloudContext>("cloud_get", { dir });
  } catch (e) {
    body.replaceChildren(h("div", { class: "notice error" }, String(e)));
    await closed;
    return false;
  }

  // Each scope's values, kept while switching between them.
  const values: Record<Scope, Record<string, string>> = { shared: { ...ctx.own }, workspace: { ...ctx.workspace } };
  let scope: Scope = "shared";
  const inputs = new Map<string, HTMLInputElement | HTMLSelectElement>();
  /** What a blank field means in the scope being edited. */
  const fallback = (key: string) => {
    const shared = values.shared[key];
    if (scope === "workspace" && shared) return `Shared: ${shared}`;
    const inherited = ctx.inherited.find((s) => s.key === key);
    return inherited && `Inherit: ${inherited.value} (from ${rel(inherited.source)})`;
  };
  const control = (f: (typeof FIELDS)[number]) => {
    let input: HTMLInputElement | HTMLSelectElement;
    if (f.choices) {
      const choices = f.choices(ctx.available);
      const own = [values.shared[f.key], values.workspace[f.key]].filter((v): v is string => !!v);
      for (const v of new Set(own)) if (!choices.some((c) => c.value === v)) choices.unshift({ value: v, text: `${v} — not found` });
      input = h("select", {}, h("option", { value: "" }), ...choices.map((c) => h("option", { value: c.value }, c.text)));
    } else {
      input = h("input", { autocomplete: "off", spellcheck: false });
    }
    input.id = `cloud-${f.key.replace(".", "-")}`;
    inputs.set(f.key, input);
    return h(
      "div",
      { class: "field" },
      h("label", { htmlFor: input.id }, f.label),
      input,
      h("div", { class: "hint" }, f.hint),
    );
  };
  /** Show `scope`'s values, and what blank means there. */
  const fill = () => {
    for (const f of FIELDS) {
      const input = inputs.get(f.key)!;
      const blank = fallback(f.key);
      if (input instanceof HTMLSelectElement) input.options[0].textContent = blank ?? "Not set";
      // Every free-text field has a placeholder.
      else input.placeholder = (blank ?? f.placeholder)!;
      input.value = values[scope][f.key] ?? "";
    }
    hint.textContent =
      scope === "shared"
        ? `Applies to tofu runs in this folder and every folder below it${ctx.repo ? `, in every checkout of ${ctx.repo.name}` : ""}. Fields left blank use the nearest folder above that sets them. Saved on this computer, not in the repo.`
        : `Only for the workspace at ${ctx.repo!.top}, e.g. to test a change against another account. A field set here wins over the shared setting for this folder; blank ones use it.`;
  };
  const keep = () => {
    for (const [k, input] of inputs) values[scope][k] = input.value.trim();
  };

  const hint = h("p", { class: "hint" });
  const scopes = ctx.repo
    ? (
        [
          ["shared", `All checkouts of ${ctx.repo.name}`],
          ["workspace", "This workspace only"],
        ] as const
      ).map(([value, text]) => {
        const radio = h("input", { type: "radio", name: "cloud-scope", value, checked: value === scope });
        radio.onchange = () => {
          keep();
          scope = value;
          fill();
        };
        return h("label", { class: "scope-option" }, radio, text);
      })
    : [];

  body.replaceChildren(
    ...(scopes.length ? [h("div", { class: "scope-picker", role: "radiogroup", ariaLabel: "Save for" }, ...scopes)] : []),
    hint,
    ...(ctx.available.errors.length
      ? [h("div", { class: "notice error" }, "Some cloud config couldn't be read:\n" + ctx.available.errors.join("\n"))]
      : []),
    ...PROVIDERS.map((p) =>
      h("section", {}, h("h3", {}, providerLogo(p.id), p.title), h("div", { class: "grid" }, ...FIELDS.filter((f) => f.provider === p).map(control))),
    ),
    error,
  );
  fill();
  save.disabled = false;
  clear.disabled = false;
  inputs.get("aws.profile")!.focus();

  clear.onclick = () => {
    for (const input of inputs.values()) input.value = "";
  };

  let saved = false;
  form.onsubmit = async (ev) => {
    ev.preventDefault();
    keep();
    save.disabled = true;
    try {
      for (const s of ctx.repo ? (["shared", "workspace"] as const) : (["shared"] as const)) {
        await invoke("cloud_set", { dir: ctx.dir, values: Object.fromEntries(FIELDS.map((f) => [f.key, values[s][f.key] ?? ""])), scope: s });
      }
      saved = true;
      dialog.close();
    } catch (e) {
      error.textContent = String(e);
      error.hidden = false;
      save.disabled = false;
    }
  };

  await closed;
  return saved;
}
