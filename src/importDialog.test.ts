import { beforeEach, describe, expect, it, vi } from "vitest";
import { deferred, flush } from "./testing/dom";

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));

import { openImportDialog, type ImportRequest } from "./importDialog";

const variable = (extra: Record<string, unknown>) => ({
  name: "v",
  typeExpr: null,
  description: null,
  default: null,
  hasDefault: false,
  sensitive: false,
  providedBy: null,
  ...extra,
});

const moduleInfo = (extra: Record<string, unknown> = {}) => ({
  variables: [],
  resources: [
    { address: "aws_s3_bucket.logs", repeated: null },
    { address: "aws_instance.web", repeated: "count" },
    { address: "aws_route53_record.r", repeated: "for_each" },
  ],
  varFiles: ["prod.tfvars", "dev.tfvars"],
  parseErrors: [],
  ...extra,
});

const dialog = () => document.querySelector<HTMLDialogElement>("dialog.import-dialog")!;
const form = () => dialog().querySelector("form")!;
const submit = () => dialog().querySelector<HTMLButtonElement>("button[type=submit]")!;
const cancel = () => [...dialog().querySelectorAll("button")].find((b) => b.textContent === "Cancel")!;

/** The control a labelled field wraps, found by its label text. */
function control<T extends HTMLElement = HTMLInputElement>(label: string): T {
  const lab = [...dialog().querySelectorAll("label")].find((l) => l.firstChild?.textContent === label)!;
  return document.getElementById(lab.htmlFor) as T;
}
const hintFor = (label: string) => control(label).closest(".field")!.querySelector(".hint");

function type(input: HTMLInputElement | HTMLSelectElement, value: string) {
  input.value = value;
  input.dispatchEvent(new Event("input"));
}

/** Submit the form as a browser would: submit event, then a method=dialog close. */
function submitForm() {
  const ev = new Event("submit", { cancelable: true });
  form().dispatchEvent(ev);
  if (!ev.defaultPrevented) dialog().close();
  return ev;
}

async function open(info: unknown = moduleInfo()) {
  tauri.invoke.mockResolvedValue(info);
  let result: ImportRequest | null | undefined;
  const p = openImportDialog("/repo/envs/prod", "envs/prod").then((r) => (result = r));
  await flush();
  return { p, result: () => result };
}

beforeEach(() => {
  tauri.invoke.mockReset();
  document.body.replaceChildren();
});

describe("openImportDialog", () => {
  it("shows a loading state while reading the module", async () => {
    const d = deferred<unknown>();
    tauri.invoke.mockReturnValue(d.promise);
    void openImportDialog("/repo/envs/prod", "envs/prod");
    expect(dialog().open).toBe(true);
    expect(dialog().querySelector("h2")!.textContent).toBe("Import into ");
    expect(dialog().querySelector(".modal-header code")!.textContent).toBe("envs/prod");
    expect(dialog().querySelector(".modal-body")!.textContent).toBe("Reading module…");
    expect(submit().disabled).toBe(true);
    expect(tauri.invoke).toHaveBeenCalledWith("module_info", { dir: "/repo/envs/prod" });
    d.resolve(moduleInfo());
    await flush();
    expect(dialog().querySelector(".modal-body")!.textContent).not.toContain("Reading module…");
  });

  it("offers resources and var files as suggestions", async () => {
    await open();
    const address = control("Address");
    expect(document.activeElement).toBe(address);
    const list = document.getElementById(address.getAttribute("list")!)!;
    expect([...list.querySelectorAll("option")].map((o) => o.value)).toEqual([
      "aws_s3_bucket.logs",
      "aws_instance.web",
      "aws_route53_record.r",
    ]);
    const varFiles = document.getElementById(control("-var-file").getAttribute("list")!)!;
    expect([...varFiles.querySelectorAll("option")].map((o) => o.value)).toEqual(["prod.tfvars", "dev.tfvars"]);

    // Address and ID are required; options aren't.
    expect(address.closest(".field")!.querySelector(".req")!.textContent).toBe(" *");
    expect(control("ID").closest(".field")!.querySelector(".req")).not.toBeNull();
    expect(control("-config").closest(".field")!.querySelector(".req")).toBeNull();
    expect(hintFor("-lock")).toBeNull();
    expect(hintFor("-config")!.textContent).toBe("Folder with the configuration to use.");
  });

  it("explains indexing for count / for_each resources", async () => {
    await open();
    const address = control("Address");
    const generic = "3 resources found in this module. Module resources: module.<name>.<type>.<name>.";
    expect(hintFor("Address")!.textContent).toBe(generic);
    type(address, " aws_instance.web ");
    expect(hintFor("Address")!.textContent).toBe("Uses count: add an index, e.g. [0].");
    type(address, "aws_route53_record.r");
    expect(hintFor("Address")!.textContent).toBe('Uses for_each: add a key, e.g. ["name"].');
    type(address, "aws_s3_bucket.logs");
    expect(hintFor("Address")!.textContent).toBe(generic);
  });

  it("enables Import only with an address and an ID", async () => {
    await open();
    expect(submit().disabled).toBe(true);
    // A submit while disabled (e.g. Enter in a field) is blocked.
    expect(submitForm().defaultPrevented).toBe(true);
    expect(dialog().open).toBe(true);

    type(control("Address"), "aws_s3_bucket.logs");
    expect(submit().disabled).toBe(true);
    type(control("ID"), "   ");
    expect(submit().disabled).toBe(true);
    type(control("ID"), "my-bucket");
    expect(submit().disabled).toBe(false);
    type(control("Address"), "");
    expect(submit().disabled).toBe(true);
  });

  it("lists variables with what they'll resolve to", async () => {
    await open(
      moduleInfo({
        variables: [
          variable({ name: "region", typeExpr: "string", providedBy: "terraform.tfvars", hasDefault: true, default: "x" }),
          variable({ name: "size", hasDefault: true, default: "3", description: "How many" }),
          variable({ name: "token", hasDefault: true, sensitive: true }),
          variable({ name: "env" }),
          variable({ name: "secret", sensitive: true }),
        ],
      }),
    );
    const rows = [...dialog().querySelectorAll<HTMLElement>(".var-row")];
    const inputs = rows.map((r) => r.querySelector("input")!);
    expect(rows.map((r) => r.querySelector(".var-name")!.textContent)).toEqual([
      "region string",
      "size",
      "tokensensitive",
      "env",
      "secretsensitive",
    ]);
    expect(inputs.map((i) => i.placeholder)).toEqual([
      "from terraform.tfvars",
      "default: 3",
      "default (sensitive)",
      "no value set — required",
      "no value set — required",
    ]);
    expect(inputs.map((i) => i.type)).toEqual(["text", "text", "password", "text", "password"]);
    expect(inputs.map((i) => i.classList.contains("unresolved"))).toEqual([false, false, false, true, true]);
    expect(rows[1].querySelector(".var-desc")!.textContent).toBe("How many");
    expect(rows[0].querySelector(".var-desc")).toBeNull();
    expect(dialog().querySelector("section .hint .error")!.textContent).toBe("2 variable(s) have no value and must be filled.");
  });

  it("omits the unresolved warning when every variable has a value", async () => {
    await open(moduleInfo({ variables: [variable({ hasDefault: true, default: "1" })] }));
    expect(dialog().querySelectorAll(".var-row")).toHaveLength(1);
    expect(dialog().querySelector("section .hint .error")).toBeNull();
  });

  it("has no Variables section for a module without variables", async () => {
    await open();
    expect([...dialog().querySelectorAll("h3")].map((h) => h.textContent)).toEqual(["Resource", "Options"]);
  });

  it("warns about unreadable files, including a failed read", async () => {
    await open(moduleInfo({ parseErrors: ["a.tf: bad", "b.tf: worse"] }));
    expect(dialog().querySelector(".notice.error")!.textContent).toBe(
      "Some files couldn't be read; suggestions may be incomplete:a.tf: bad\nb.tf: worse",
    );
    dialog().close();

    tauri.invoke.mockRejectedValue("no such folder");
    void openImportDialog("/gone", "gone");
    await flush();
    expect(dialog().querySelector(".notice.error")!.textContent).toContain("no such folder");
    expect(hintFor("Address")!.textContent).toMatch(/^0 resources found/);
  });

  it("resolves null on Cancel and removes the dialog", async () => {
    const { p } = await open();
    cancel().click();
    await expect(p).resolves.toBeNull();
    expect(document.querySelector("dialog")).toBeNull();
  });

  it("resolves the minimal request, leaving blank options out", async () => {
    const { p } = await open(moduleInfo({ variables: [variable({ name: "a" }), variable({ name: "b" })] }));
    type(control("Address"), "  aws_s3_bucket.logs ");
    type(control("ID"), " my-bucket ");
    type(control("-var-file"), "   ");
    type(control("-parallelism"), "0");
    const inputs = [...dialog().querySelectorAll<HTMLInputElement>(".var-row input")];
    inputs[1].value = " spaced ";
    submitForm();
    await expect(p).resolves.toEqual({
      address: "aws_s3_bucket.logs",
      id: "my-bucket",
      vars: [{ name: "b", value: " spaced " }],
      varFile: null,
      config: null,
      lock: null,
      lockTimeout: null,
      parallelism: null,
      ignoreRemoteVersion: false,
    });
    expect(document.querySelector("dialog")).toBeNull();
  });

  it("passes every filled option through", async () => {
    const { p } = await open();
    type(control("Address"), "aws_instance.web[0]");
    type(control("ID"), "i-123");
    type(control("-var-file"), " prod.tfvars ");
    type(control("-config"), "../shared");
    type(control<HTMLSelectElement>("-lock"), "false");
    type(control("-lock-timeout"), "30s");
    type(control("-parallelism"), "4");
    dialog().querySelector<HTMLInputElement>("input[type=checkbox]")!.checked = true;
    submitForm();
    await expect(p).resolves.toEqual({
      address: "aws_instance.web[0]",
      id: "i-123",
      vars: [],
      varFile: "prod.tfvars",
      config: "../shared",
      lock: false,
      lockTimeout: "30s",
      parallelism: 4,
      ignoreRemoteVersion: true,
    });
  });

  it("maps -lock true and ignores a non-numeric parallelism", async () => {
    const { p } = await open();
    type(control("Address"), "a.b");
    type(control("ID"), "x");
    type(control<HTMLSelectElement>("-lock"), "true");
    // A number input can't hold text, so simulate a bad value directly.
    Object.defineProperty(control("-parallelism"), "value", { value: "abc" });
    submitForm();
    await expect(p).resolves.toMatchObject({ lock: true, parallelism: null });
  });
});
