// Cloud settings for a folder: which AWS profile, gcloud configuration or
// Azure subscription tofu uses there and in every folder below it. Choices
// come from the providers' own CLI config; settings are kept by Gestalt, not
// in the repo.

import { invoke } from "@tauri-apps/api/core";
import { providerLogos, type ProviderId } from "./icons";

/** A setting in effect, and the folder it was set on. */
export interface CloudSetting {
  key: string;
  value: string;
  source: string;
}

interface Available {
  awsProfiles: { name: string; region: string | null }[];
  gcpConfigurations: { name: string; project: string | null; account: string | null; active: boolean }[];
  azureSubscriptions: { id: string; name: string; tenantId: string; isDefault: boolean }[];
  errors: string[];
}

interface CloudContext {
  dir: string;
  own: Record<string, string>;
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

/** Providers a folder's own settings name, for its badges. */
export function cloudBadges(own: Record<string, string>): { id: ProviderId; title: string }[] {
  return PROVIDERS.filter((p) => p.fields.some((f) => own[f.key])).map((p) => ({
    id: p.id,
    title: p.fields
      .filter((f) => own[f.key])
      .map((f) => `${p.title} ${f.label.toLowerCase()}: ${own[f.key]}`)
      .join("\n"),
  }));
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

  const inputs = new Map<string, HTMLInputElement | HTMLSelectElement>();
  const control = (f: (typeof FIELDS)[number]) => {
    const own = ctx.own[f.key] ?? "";
    const inherited = ctx.inherited.find((s) => s.key === f.key);
    const from = inherited && `${inherited.value} (from ${rel(inherited.source)})`;
    let input: HTMLInputElement | HTMLSelectElement;
    if (f.choices) {
      const choices = f.choices(ctx.available);
      if (own && !choices.some((c) => c.value === own)) choices.unshift({ value: own, text: `${own} — not found` });
      input = h(
        "select",
        {},
        h("option", { value: "" }, from ? `Inherit: ${from}` : "Not set"),
        ...choices.map((c) => h("option", { value: c.value }, c.text)),
      );
    } else {
      input = h("input", { placeholder: from ? `Inherit: ${from}` : f.placeholder, autocomplete: "off", spellcheck: false });
    }
    input.value = own;
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

  body.replaceChildren(
    h(
      "p",
      { class: "hint" },
      "Applies to tofu runs in this folder and every folder below it. Fields left to inherit use the nearest folder above that sets them. Saved on this computer, not in the repo.",
    ),
    ...(ctx.available.errors.length
      ? [h("div", { class: "notice error" }, "Some cloud config couldn't be read:\n" + ctx.available.errors.join("\n"))]
      : []),
    ...PROVIDERS.map((p) =>
      h("section", {}, h("h3", {}, providerLogo(p.id), p.title), h("div", { class: "grid" }, ...FIELDS.filter((f) => f.provider === p).map(control))),
    ),
    error,
  );
  save.disabled = false;
  clear.disabled = false;
  inputs.get("aws.profile")!.focus();

  clear.onclick = () => {
    for (const input of inputs.values()) input.value = "";
  };

  let saved = false;
  form.onsubmit = async (ev) => {
    ev.preventDefault();
    const values = Object.fromEntries([...inputs].map(([k, input]) => [k, input.value.trim()]));
    save.disabled = true;
    try {
      await invoke("cloud_set", { dir: ctx.dir, values });
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
