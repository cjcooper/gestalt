import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invokeRouter } from "./testing/dom";

const tauri = vi.hoisted(() => ({ invoke: vi.fn(), ask: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: tauri.ask }));

import { openEnvEditor } from "./envEditor";

type Handlers = Record<string, (args?: Record<string, unknown>) => unknown>;

const ctx = (extra: Record<string, unknown> = {}) => ({
  relPath: "envs/prod/.gestalt.env",
  exists: true,
  content: "A=1\n",
  inherited: [],
  inheritedUnset: [],
  parentErrors: [],
  ...extra,
});

let handlers: Handlers;
function route(h: Handlers) {
  handlers = { env_file_read: () => ctx(), env_file_validate: () => null, ...h };
  tauri.invoke.mockImplementation(invokeRouter(new Proxy({}, { get: (_, k: string) => handlers[k] })));
}

const settle = () => vi.advanceTimersByTimeAsync(0);
const dialog = () => document.querySelector<HTMLDialogElement>("dialog.env-editor")!;
const q = <T extends HTMLElement = HTMLElement>(sel: string) => dialog().querySelector<T>(sel)!;
const textarea = () => q<HTMLTextAreaElement>("textarea");
const button = (label: string) =>
  [...dialog().querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent === label)!;
const status = () => q(".editor-status");
const saveError = () => q(".editor-main .notice");

/** Open the editor and let its first validation finish. */
async function open(h: Handlers = {}) {
  route(h);
  let result: boolean | undefined;
  const p = openEnvEditor("/repo/envs/prod").then((r) => (result = r));
  await settle();
  return { p, result: () => result };
}

function type(value: string) {
  textarea().value = value;
  textarea().dispatchEvent(new Event("input"));
}

beforeEach(() => {
  vi.useFakeTimers();
  tauri.invoke.mockReset();
  tauri.ask.mockReset();
  document.body.replaceChildren();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("openEnvEditor", () => {
  it("reports a read failure and resolves false", async () => {
    tauri.ask.mockResolvedValue(true);
    route({
      env_file_read: () => {
        throw "permission denied";
      },
    });
    await expect(openEnvEditor("/x")).resolves.toBe(false);
    expect(tauri.invoke).toHaveBeenCalledWith("env_file_read", { dir: "/x" });
    expect(tauri.ask).toHaveBeenCalledWith("permission denied", {
      title: "Can't edit .gestalt.env",
      kind: "error",
      okLabel: "OK",
      cancelLabel: "Close",
    });
    expect(document.querySelector("dialog")).toBeNull();
  });

  it("edits an existing file", async () => {
    await open();
    expect(dialog().open).toBe(true);
    expect(q("h2").textContent).toBe("Edit");
    expect(q(".modal-header code").textContent).toBe("envs/prod/.gestalt.env");
    expect(textarea().value).toBe("A=1\n");
    expect(document.activeElement).toBe(textarea());
    expect(button("Save")).toBeDefined();
    expect(button("Delete file").hidden).toBe(false);
    expect(tauri.invoke).toHaveBeenCalledWith("env_file_validate", { content: "A=1\n" });
    expect(status().className).toBe("editor-status ok");
    expect(status().textContent).toBe("✓ Valid");
  });

  it("starts a new file from a template with the cursor at the end", async () => {
    await open({ env_file_read: () => ctx({ exists: false, content: "" }) });
    expect(q("h2").textContent).toBe("Create");
    expect(button("Create")).toBeDefined();
    expect(button("Delete file").hidden).toBe(true);
    const value = textarea().value;
    expect(value).toMatch(/^# Environment for tofu runs in this folder/);
    expect(value).toContain('# KEY="a $VAR\\tb"');
    expect(value).toContain("${VAR}");
    expect(textarea().selectionStart).toBe(value.length);
    expect(textarea().selectionEnd).toBe(value.length);
  });

  it("shows inherited variables, overrides and unsets", async () => {
    await open({
      env_file_read: () =>
        ctx({
          content: "export A=2\nunset C bad-name D\n",
          inherited: [
            { name: "A", value: "1", source: "envs/.gestalt.env" },
            { name: "B", value: "", source: ".gestalt.env" },
          ],
          inheritedUnset: [{ name: "Z", source: "envs/.gestalt.env" }],
          parentErrors: ["envs/.gestalt.env: line 3: bad", "other"],
        }),
    });
    const side = q("aside.inherited");
    expect(side.querySelector(".notice.error")!.textContent).toBe("envs/.gestalt.env: line 3: bad\nother");
    const rows = [...side.querySelectorAll<HTMLElement>(".inherited-row")];
    expect(rows.map((r) => r.className)).toEqual(["inherited-row overridden", "inherited-row", "inherited-row unset"]);
    expect(rows.map((r) => r.textContent)).toEqual(["A1overridden here", 'B"".gestalt.env', "Zunsetenvs/.gestalt.env"]);
    expect(rows[0].title).toBe("set in envs/.gestalt.env");

    // Typing updates which ones are overridden.
    type("B=x\n");
    expect([...side.querySelectorAll(".inherited-row")].map((r) => r.className)).toEqual([
      "inherited-row",
      "inherited-row overridden",
      "inherited-row unset",
    ]);
    type("unset A B\n");
    expect([...side.querySelectorAll(".inherited-row.overridden")]).toHaveLength(2);
  });

  it("says when nothing is inherited", async () => {
    await open();
    expect(q("aside .inherited-list").textContent).toBe("Nothing inherited — no parent folder has a .gestalt.env.");
    expect(q("aside").querySelector(".notice")).toBeNull();
  });

  it("validates as you type, debounced", async () => {
    await open({ env_file_validate: (a) => (String(a!.content).includes("=") ? null : "line 1: expected KEY=value") });
    tauri.invoke.mockClear();
    type("oops");
    type("oops2");
    await vi.advanceTimersByTimeAsync(199);
    expect(tauri.invoke).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1);
    expect(tauri.invoke).toHaveBeenCalledTimes(1);
    expect(tauri.invoke).toHaveBeenCalledWith("env_file_validate", { content: "oops2" });
    expect(status().className).toBe("editor-status error");
    expect(status().textContent).toBe("✕ line 1: expected KEY=value");
    expect(button("Save").disabled).toBe(true);

    // Saving while invalid does nothing: the button is disabled, and the
    // handler itself refuses too (e.g. if invoked directly).
    button("Save").click();
    await button("Save").onclick!.call(button("Save"), new PointerEvent("click"));
    await settle();
    expect(tauri.invoke).toHaveBeenCalledTimes(1);

    type("A=1");
    await vi.advanceTimersByTimeAsync(200);
    expect(button("Save").disabled).toBe(false);
    expect(status().textContent).toBe("✓ Valid");
  });

  it("inserts spaces on Tab and saves on Cmd/Ctrl+S", async () => {
    await open({ env_file_save: () => undefined });
    textarea().value = "AB";
    textarea().setSelectionRange(1, 1);
    const tab = new KeyboardEvent("keydown", { key: "Tab", cancelable: true });
    textarea().dispatchEvent(tab);
    expect(tab.defaultPrevented).toBe(true);
    expect(textarea().value).toBe("A  B");
    expect(textarea().selectionStart).toBe(3);

    const shiftTab = new KeyboardEvent("keydown", { key: "Tab", shiftKey: true, cancelable: true });
    textarea().dispatchEvent(shiftTab);
    expect(shiftTab.defaultPrevented).toBe(false);
    expect(textarea().value).toBe("A  B");

    const plainS = new KeyboardEvent("keydown", { key: "s", cancelable: true });
    textarea().dispatchEvent(plainS);
    expect(plainS.defaultPrevented).toBe(false);

    await vi.advanceTimersByTimeAsync(200);
    const save = vi.spyOn(button("Save"), "click");
    const ctrlS = new KeyboardEvent("keydown", { key: "S", ctrlKey: true, cancelable: true });
    textarea().dispatchEvent(ctrlS);
    expect(ctrlS.defaultPrevented).toBe(true);
    textarea().dispatchEvent(new KeyboardEvent("keydown", { key: "s", metaKey: true, cancelable: true }));
    expect(save).toHaveBeenCalledTimes(2);
  });

  it("saves, sending the content it started from, and resolves true", async () => {
    const { p, result } = await open({ env_file_save: () => undefined });
    type("A=2\n");
    await vi.advanceTimersByTimeAsync(200);
    button("Save").click();
    await p;
    expect(tauri.invoke).toHaveBeenCalledWith("env_file_save", {
      dir: "/repo/envs/prod",
      content: "A=2\n",
      expected: "A=1\n",
    });
    expect(result()).toBe(true);
    expect(document.querySelector("dialog")).toBeNull();
  });

  it("creates a new file expecting none to exist", async () => {
    const { p } = await open({ env_file_read: () => ctx({ exists: false, content: "" }), env_file_save: () => undefined });
    button("Create").click();
    await expect(p).resolves.toBe(true);
    expect(tauri.invoke).toHaveBeenCalledWith("env_file_save", expect.objectContaining({ expected: null }));
  });

  it("shows a save error and stays open; typing hides it", async () => {
    const { result } = await open({
      env_file_save: () => {
        throw "changed on disk";
      },
    });
    button("Save").click();
    await settle();
    expect(saveError().hidden).toBe(false);
    expect(saveError().textContent).toBe("changed on disk");
    expect(dialog()).not.toBeNull();
    expect(result()).toBeUndefined();
    type("A=3");
    expect(saveError().hidden).toBe(true);
  });

  it("closes on Cancel without asking when nothing changed", async () => {
    const { p } = await open();
    button("Cancel").click();
    await expect(p).resolves.toBe(false);
    expect(tauri.ask).not.toHaveBeenCalled();
    expect(document.querySelector("dialog")).toBeNull();
  });

  it("asks before discarding changes (Cancel or Esc)", async () => {
    const { p, result } = await open();
    type("A=9");
    tauri.ask.mockResolvedValueOnce(false);
    button("Cancel").click();
    await settle();
    expect(tauri.ask).toHaveBeenCalledWith("Discard your unsaved changes?", {
      title: ".gestalt.env",
      kind: "warning",
      okLabel: "Discard",
      cancelLabel: "Keep editing",
    });
    expect(result()).toBeUndefined();
    expect(dialog().open).toBe(true);

    tauri.ask.mockResolvedValueOnce(true);
    const esc = new Event("cancel", { cancelable: true });
    dialog().dispatchEvent(esc);
    expect(esc.defaultPrevented).toBe(true);
    await expect(p).resolves.toBe(false);
    expect(tauri.ask).toHaveBeenCalledTimes(2);
  });

  it("deletes the file after confirming", async () => {
    const { p, result } = await open({ env_file_delete: () => undefined });
    tauri.ask.mockResolvedValueOnce(false);
    button("Delete file").click();
    await settle();
    expect(tauri.ask).toHaveBeenCalledWith(
      "Delete envs/prod/.gestalt.env?\n\nRuns in this folder and below will no longer get its variables.",
      { title: "Delete .gestalt.env", kind: "warning", okLabel: "Delete", cancelLabel: "Cancel" },
    );
    expect(tauri.invoke).not.toHaveBeenCalledWith("env_file_delete", expect.anything());
    expect(result()).toBeUndefined();

    tauri.ask.mockResolvedValueOnce(true);
    type("A=edited");
    button("Delete file").click();
    await expect(p).resolves.toBe(true);
    // Deletes against what's on disk, not the edited text.
    expect(tauri.invoke).toHaveBeenCalledWith("env_file_delete", { dir: "/repo/envs/prod", expected: "A=1\n" });
  });

  it("shows a delete error and stays open", async () => {
    const { result } = await open({
      env_file_delete: () => {
        throw "gone already";
      },
    });
    tauri.ask.mockResolvedValue(true);
    button("Delete file").click();
    await settle();
    expect(saveError().hidden).toBe(false);
    expect(saveError().textContent).toBe("gone already");
    expect(result()).toBeUndefined();
  });
});
