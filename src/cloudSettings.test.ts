import { beforeEach, describe, expect, it, vi } from "vitest";
import { deferred, flush } from "./testing/dom";

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));

import { cloudBadges, openCloudSettings, providerLogo } from "./cloudSettings";

const available = () => ({
  awsProfiles: [
    { name: "default", region: "us-east-1" },
    { name: "prd", region: null },
  ],
  gcpConfigurations: [
    { name: "default", project: null, account: null, active: false },
    { name: "work", project: "work-123", account: "me@example.com", active: true },
  ],
  azureSubscriptions: [{ id: "s-1", name: "Main", tenantId: "t-1", isDefault: true }, { id: "s-2", name: "Dev", tenantId: "t-2", isDefault: false }],
  errors: [] as string[],
});

const context = (extra: Record<string, unknown> = {}) => ({
  dir: "/real/repo/envs",
  repo: null,
  own: {},
  workspace: {},
  inherited: [],
  available: available(),
  ...extra,
});

const dialog = () => document.querySelector<HTMLDialogElement>("dialog.cloud-dialog");
const button = (text: string) => [...dialog()!.querySelectorAll("button")].find((b) => b.textContent === text)!;
const input = (key: string) => document.getElementById(`cloud-${key.replace(".", "-")}`) as HTMLInputElement & HTMLSelectElement;
const options = (key: string) => [...input(key).querySelectorAll("option")].map((o) => [o.value, o.textContent]);

function submit() {
  dialog()!.querySelector("form")!.dispatchEvent(new Event("submit", { cancelable: true }));
}

/** Open the dialog for /repo/envs with `ctx` as what cloud_get returns. */
async function open(ctx: unknown = context(), set: () => unknown = () => undefined) {
  tauri.invoke.mockImplementation(async (cmd: string) => (cmd === "cloud_get" ? ctx : set()));
  let result: boolean | undefined;
  const p = openCloudSettings("/repo/envs", "envs", (p) => p.replace("/real/", "")).then((r) => (result = r));
  await flush();
  return { p, result: () => result };
}

beforeEach(() => {
  tauri.invoke.mockReset();
  document.body.replaceChildren();
});

describe("cloudBadges", () => {
  it("lists the providers a folder sets, with what it sets", () => {
    expect(cloudBadges({ shared: {}, workspace: {} })).toEqual([]);
    expect(cloudBadges({ shared: { "aws.profile": "prd", "aws.region": "eu-west-1", "azure.tenant": "t" }, workspace: {} })).toEqual([
      { id: "aws", title: "AWS profile: prd\nAWS region: eu-west-1" },
      { id: "azure", title: "Azure tenant: t" },
    ]);
  });

  it("shows a workspace's overrides over the shared settings", () => {
    expect(cloudBadges({ shared: { "aws.profile": "prd" }, workspace: { "aws.profile": "sb", "gcp.project": "p" } })).toEqual([
      { id: "aws", title: "AWS profile: sb (this workspace only)" },
      { id: "gcp", title: "Google Cloud project: p (this workspace only)" },
    ]);
  });
});

describe("providerLogo", () => {
  it("draws the provider's mark, titled if asked", () => {
    const azure = providerLogo("azure", "Azure");
    expect(azure.className).toBe("cloud-logo azure");
    expect(azure.title).toBe("Azure");
    expect(azure.querySelector("svg")!.getAttribute("fill")).toBe("currentColor");
    expect(providerLogo("aws").title).toBe("");
  });
});

describe("openCloudSettings", () => {
  it("offers the providers' profiles, inheriting by default", async () => {
    await open(
      context({
        inherited: [
          { key: "aws.profile", value: "dev", source: "/real/repo" },
          { key: "aws.region", value: "us-west-2", source: "/real/repo" },
        ],
      }),
    );
    expect(tauri.invoke).toHaveBeenCalledWith("cloud_get", { dir: "/repo/envs" });
    expect(dialog()!.querySelector(".modal-header code")!.textContent).toBe("envs");
    expect(options("aws.profile")).toEqual([
      ["", "Inherit: dev (from repo)"],
      ["default", "default (us-east-1)"],
      ["prd", "prd"],
    ]);
    expect(input("aws.profile").value).toBe("");
    expect(document.activeElement).toBe(input("aws.profile"));
    expect(input("aws.region").placeholder).toBe("Inherit: us-west-2 (from repo)");
    expect(options("gcp.configuration")).toEqual([
      ["", "Not set"],
      ["default", "default"],
      ["work", "work (work-123) — active"],
    ]);
    expect(input("gcp.project").placeholder).toBe("the configuration's project");
    expect(options("azure.subscription")).toEqual([
      ["", "Not set"],
      ["s-1", "Main (s-1) — default"],
      ["s-2", "Dev (s-2)"],
    ]);
    expect(dialog()!.querySelector(".notice:not([hidden])")).toBeNull();
    expect(dialog()!.querySelector(".scope-picker")).toBeNull();
    expect(dialog()!.querySelector(".modal-body > .hint")!.textContent).toMatch(/^Applies to tofu runs in this folder and every folder below it\. /);
  });

  describe("in a git repository", () => {
    const repoCtx = (extra: Record<string, unknown> = {}) =>
      context({
        repo: { name: "infra", top: "/real/repo" },
        own: { "aws.profile": "prd" },
        workspace: { "aws.region": "eu-west-1", "gcp.configuration": "old" },
        inherited: [{ key: "aws.region", value: "us-west-2", source: "/real/repo" }],
        ...extra,
      });
    const scopeRadio = (value: string) => dialog()!.querySelector<HTMLInputElement>(`input[name=cloud-scope][value=${value}]`)!;
    const pick = (value: string) => {
      scopeRadio(value).checked = true;
      scopeRadio(value).dispatchEvent(new Event("change"));
    };
    const hintText = () => dialog()!.querySelector(".modal-body > .hint")!.textContent;

    it("edits the settings every checkout shares, or this workspace's", async () => {
      await open(repoCtx());
      expect([...dialog()!.querySelectorAll(".scope-option")].map((l) => l.textContent)).toEqual(["All checkouts of infra", "This workspace only"]);
      expect(scopeRadio("shared").checked).toBe(true);
      expect(hintText()).toMatch(/in every checkout of infra\./);
      expect(input("aws.profile").value).toBe("prd");
      expect(input("aws.region").placeholder).toBe("Inherit: us-west-2 (from repo)");
      // A workspace value no longer found is offered too.
      expect(options("gcp.configuration")[1]).toEqual(["old", "old — not found"]);

      input("aws.profile").value = "default";
      pick("workspace");
      expect(hintText()).toMatch(/^Only for the workspace at \/real\/repo/);
      expect(input("aws.region").value).toBe("eu-west-1");
      expect(input("aws.profile").value).toBe("");
      // Blank here means the shared value, as just edited.
      expect(options("aws.profile")[0]).toEqual(["", "Shared: default"]);
      expect(input("gcp.project").placeholder).toBe("the configuration's project");

      input("aws.profile").value = "prd";
      pick("shared");
      expect(input("aws.profile").value).toBe("default");
      expect(options("aws.profile")[0]).toEqual(["", "Not set"]);
    });

    it("saves the workspace's values as they were when only the shared ones changed", async () => {
      const { p } = await open(repoCtx());
      input("aws.profile").value = "";
      submit();
      await p;
      const last = tauri.invoke.mock.lastCall![1];
      expect(last.scope).toBe("workspace");
      expect(last.values).toEqual({
        "aws.profile": "",
        "aws.region": "eu-west-1",
        "gcp.configuration": "old",
        "gcp.project": "",
        "azure.subscription": "",
        "azure.tenant": "",
      });
    });

    it("saves both", async () => {
      const { p } = await open(repoCtx());
      pick("workspace");
      input("aws.profile").value = "prd";
      submit();
      await p;
      const sets = tauri.invoke.mock.calls.filter(([cmd]) => cmd === "cloud_set").map(([, a]) => [a.scope, a.values["aws.profile"], a.values["aws.region"]]);
      expect(sets).toEqual([
        ["shared", "prd", ""],
        ["workspace", "prd", "eu-west-1"],
      ]);
    });
  });

  it("shows the folder's own values, including ones no longer found", async () => {
    await open(context({ own: { "aws.profile": "gone", "gcp.configuration": "work", "aws.region": "eu-west-1" } }));
    expect(input("aws.profile").value).toBe("gone");
    expect(options("aws.profile")[1]).toEqual(["gone", "gone — not found"]);
    expect(input("gcp.configuration").value).toBe("work");
    expect(input("aws.region").value).toBe("eu-west-1");
  });

  it("saves every field, blank meaning inherit, to the folder it read", async () => {
    const { p, result } = await open(context({ own: { "aws.region": "eu-west-1" } }));
    input("aws.profile").value = "prd";
    input("gcp.project").value = "  my-proj ";
    submit();
    await p;
    expect(result()).toBe(true);
    expect(tauri.invoke).toHaveBeenLastCalledWith("cloud_set", {
      dir: "/real/repo/envs",
      values: {
        "aws.profile": "prd",
        "aws.region": "eu-west-1",
        "gcp.configuration": "",
        "gcp.project": "my-proj",
        "azure.subscription": "",
        "azure.tenant": "",
      },
      scope: "shared",
    });
    // Outside a repository there's nothing else to save, and no scope to pick.
    expect(tauri.invoke).toHaveBeenCalledTimes(2);
    expect(dialog()).toBeNull();
  });

  it("clears every field before saving", async () => {
    const { p } = await open(context({ own: { "aws.profile": "prd", "azure.tenant": "t" } }));
    button("Clear all").click();
    expect(input("aws.profile").value).toBe("");
    expect(input("azure.tenant").value).toBe("");
    submit();
    await p;
    const values = tauri.invoke.mock.lastCall![1].values;
    expect(Object.values(values).every((v) => v === "")).toBe(true);
  });

  it("keeps the dialog open when saving fails", async () => {
    const { p, result } = await open(context(), () => {
      throw "disk full";
    });
    submit();
    await flush();
    const error = dialog()!.querySelector<HTMLElement>(".modal-body > .notice.error")!;
    expect(error.hidden).toBe(false);
    expect(error.textContent).toBe("disk full");
    expect(button("Save").disabled).toBe(false);
    button("Cancel").click();
    await p;
    expect(result()).toBe(false);
  });

  it("disables Save while saving", async () => {
    const saving = deferred<void>();
    const { p } = await open(context(), () => saving.promise);
    submit();
    expect(button("Save").disabled).toBe(true);
    saving.resolve();
    await p;
  });

  it("reports config files that couldn't be read", async () => {
    const a = available();
    a.errors = ["/home/.aws/config: permission denied"];
    a.awsProfiles = [];
    await open(context({ available: a }));
    expect(dialog()!.querySelector(".notice.error")!.textContent).toBe(
      "Some cloud config couldn't be read:\n/home/.aws/config: permission denied",
    );
    expect(options("aws.profile")).toEqual([["", "Not set"]]);
  });

  it("shows why the settings couldn't be read, and closes without saving", async () => {
    tauri.invoke.mockRejectedValue("not a folder");
    let result: boolean | undefined;
    const p = openCloudSettings("/x", "x", (p) => p).then((r) => (result = r));
    await flush();
    expect(dialog()!.querySelector(".notice.error")!.textContent).toBe("not a folder");
    expect(button("Save").disabled).toBe(true);
    expect(button("Clear all").disabled).toBe(true);
    button("Cancel").click();
    await p;
    expect(result).toBe(false);
    expect(dialog()).toBeNull();
  });

  it("offers no account on a repository's top", async () => {
    const top = context({ dir: "/real/repo", repo: { name: "infra", top: "/real/repo" } });
    const { p, result } = await open(top);
    expect(dialog()!.querySelector(".notice")!.textContent).toBe(
      "This is the top of infra, which can't have a cloud account: it would apply to every module in the repository. Choose one on the folders below it instead.",
    );
    expect(input("aws.profile")).toBeNull();
    expect(button("Save").hidden).toBe(true);
    expect(button("Clear all").disabled).toBe(true);
    button("Cancel").click();
    await p;
    expect(result()).toBe(false);
  });

  it("clears settings an earlier version kept on a repository's top", async () => {
    const top = context({ dir: "/real/repo", repo: { name: "infra", top: "/real/repo" }, workspace: { "aws.profile": "sb" } });
    let fail = true;
    const { p, result } = await open(top, () => {
      if (fail) throw "disk full";
    });
    expect(dialog()!.querySelector(".notice")!.textContent).toMatch(/earlier version of Gestalt are no longer used\.$/);
    button("Clear all").click();
    await flush();
    expect(dialog()!.querySelector<HTMLElement>(".notice.error")!.hidden).toBe(false);
    expect(dialog()!.querySelector(".notice.error")!.textContent).toBe("disk full");
    fail = false;
    tauri.invoke.mockClear();
    button("Clear all").click();
    await p;
    expect(tauri.invoke.mock.calls).toEqual([
      ["cloud_set", { dir: "/real/repo", values: {}, scope: "shared" }],
      ["cloud_set", { dir: "/real/repo", values: {}, scope: "workspace" }],
    ]);
    // Nothing chosen, so nothing for callers to refresh beyond the event.
    expect(result()).toBe(false);
  });
});
