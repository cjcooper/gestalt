import { beforeEach, describe, expect, it, vi } from "vitest";
import { deferred, flush } from "./testing/dom";

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));

import { cloudBadges, cloudParts, cloudSummary, openCloudSettings, providerLogo } from "./cloudSettings";

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
  own: {},
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

describe("cloudSummary", () => {
  it("names what's in effect per provider", () => {
    expect(cloudSummary([])).toBe("");
    expect(
      cloudSummary([
        { key: "aws.profile", value: "prd", source: "/r" },
        { key: "aws.region", value: "eu-west-1", source: "/r" },
        { key: "azure.subscription", value: "s-1", source: "/r" },
      ]),
    ).toBe("AWS prd · eu-west-1 | Azure s-1");
    expect(cloudSummary([{ key: "gcp.project", value: "p", source: "/r" }])).toBe("Google Cloud p");
  });
});

describe("cloudBadges", () => {
  it("lists the providers a folder sets, with what it sets", () => {
    expect(cloudBadges({})).toEqual([]);
    expect(cloudBadges({ "aws.profile": "prd", "aws.region": "eu-west-1", "azure.tenant": "t" })).toEqual([
      { id: "aws", title: "AWS profile: prd\nAWS region: eu-west-1" },
      { id: "azure", title: "Azure tenant: t" },
    ]);
  });
});

describe("cloudParts", () => {
  it("groups values by provider", () => {
    expect(cloudParts([{ key: "gcp.configuration", value: "c", source: "/r" }, { key: "gcp.project", value: "p", source: "/r" }])).toEqual([
      { id: "gcp", title: "Google Cloud", text: "c · p" },
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
    });
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
});
