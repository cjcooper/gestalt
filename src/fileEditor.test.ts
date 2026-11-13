import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { EditorView } from "@codemirror/view";
import { deferred, flush, invokeRouter } from "./testing/dom";
import { updateSettings } from "./settings";

type CloseEv = { preventDefault: () => void };
const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  ask: vi.fn(),
  message: vi.fn(),
  emit: vi.fn(),
  emitTo: vi.fn(),
  win: {
    label: "editor-1",
    destroy: vi.fn(),
    close: vi.fn(),
    onCloseRequested: vi.fn(),
    listen: vi.fn(),
  },
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ emit: tauri.emit, emitTo: tauri.emitTo }));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => tauri.win }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: tauri.ask, message: tauri.message }));

import { mountFileEditor } from "./fileEditor";

type Handlers = Record<string, (args?: Record<string, unknown>) => unknown>;
const PATH = "/repo/envs/prod/main.tf";
const CONFLICT = `${PATH} was changed on disk since you opened it.`;

let handlers: Handlers;
let saves: Record<string, unknown>[];
function route(h: Handlers = {}) {
  saves = [];
  handlers = {
    file_read: () => ({ relPath: "envs/prod/main.tf", content: "a = 1\n", isModule: true, linkTarget: null }),
    file_save: (args) => {
      saves.push(args!);
    },
    ...h,
  };
  tauri.invoke.mockImplementation(invokeRouter(new Proxy({}, { get: (_, k: string) => handlers[k] })));
}

const q = <T extends HTMLElement = HTMLElement>(sel: string) => document.body.querySelector<T>(sel)!;
const view = () => EditorView.findFromDOM(q(".cm-editor"))!;
const text = () => view().state.doc.toString();
const status = () => q(".file-status");
const error = () => q(".notice");
const saveBtn = () => q<HTMLButtonElement>("[data-action=save]");

async function mount(h: Handlers = {}, path = PATH, owner?: string) {
  route(h);
  await mountFileEditor(document.body, path, owner);
}

function type(s: string) {
  view().dispatch({ changes: { from: view().state.doc.length, insert: s } });
}
const blurEditor = () => view().contentDOM.dispatchEvent(new FocusEvent("blur"));
const blurWindow = () => window.dispatchEvent(new FocusEvent("blur"));
const key = (k: string, extra: KeyboardEventInit = {}, target: Element = view().contentDOM) => {
  const ev = new KeyboardEvent("keydown", { key: k, ctrlKey: true, bubbles: true, cancelable: true, ...extra });
  target.dispatchEvent(ev);
  return ev;
};

/** The window's close button / ⌘W: resolves whether the close was refused. */
async function requestClose(): Promise<boolean> {
  const handler = tauri.win.onCloseRequested.mock.calls[0][0] as (ev: CloseEv) => Promise<void>;
  const ev = { preventDefault: vi.fn() };
  await handler(ev);
  return ev.preventDefault.mock.calls.length > 0;
}

/** The main window asking this editor to close. */
async function mainAsksToClose() {
  const [event, handler] = tauri.win.listen.mock.calls[0] as [string, () => Promise<void>];
  expect(event).toBe("editor://close");
  await handler();
}

// Each mount adds document/window listeners; remove them between tests.
let added: [EventTarget, string, EventListenerOrEventListenerObject][] = [];
beforeEach(() => {
  for (const t of [document, window] as EventTarget[]) {
    const real = t.addEventListener.bind(t);
    vi.spyOn(t, "addEventListener").mockImplementation(
      (type: string, l: EventListenerOrEventListenerObject | null, o?: boolean | AddEventListenerOptions) => {
        if (l) added.push([t, type, l]);
        real(type, l, o);
      },
    );
  }
  for (const f of [tauri.invoke, tauri.ask, tauri.message, tauri.emit, tauri.emitTo, tauri.win.destroy, tauri.win.close, tauri.win.onCloseRequested, tauri.win.listen])
    f.mockReset();
  tauri.win.onCloseRequested.mockResolvedValue(() => {});
  tauri.win.listen.mockResolvedValue(() => {});
  localStorage.clear();
  document.body.replaceChildren();
  document.title = "";
});

afterEach(() => {
  for (const [t, type, l] of added) t.removeEventListener(type, l);
  added = [];
});

describe("mountFileEditor", () => {
  it("reports a file that can't be opened and closes the window", async () => {
    await mount({
      file_read: () => {
        throw "x isn't a text file.";
      },
    });
    expect(tauri.message).toHaveBeenCalledWith("x isn't a text file.", { title: "Can't open file", kind: "error" });
    expect(tauri.win.destroy).toHaveBeenCalled();
    expect(document.body.children).toHaveLength(0);
  });

  it("shows the file with its language, focused, nothing to save", async () => {
    await mount();
    expect(tauri.invoke).toHaveBeenCalledWith("file_read", { path: PATH });
    expect(document.title).toBe("envs/prod/main.tf");
    expect(q(".file-lang").textContent).toBe("HCL");
    expect(text()).toBe("a = 1\n");
    expect(document.activeElement).toBe(view().contentDOM);
    expect(status().textContent).toBe("");
    expect(saveBtn().disabled).toBe(true);
    expect(q(".hint").textContent).toBe("Saves automatically when you close this window or click away.");
    expect(q(".cm-content").innerHTML).toContain("<span");
  });

  it("has a Save icon button in an action bar at the top", async () => {
    await mount();
    expect(document.body.querySelector(":scope > :not([hidden])")).toBe(q(".action-bar"));
    expect(saveBtn().closest(".action-bar")).not.toBeNull();
    expect(saveBtn().querySelector("svg")).not.toBeNull();
    expect(saveBtn().getAttribute("aria-label")).toBe("Save");
    expect(saveBtn().title).toBe("Save (⌘S)");
    // No Close button, no Apply: Save, then Plan for this module folder.
    expect([...document.querySelectorAll("button")].map((b) => b.dataset.action)).toEqual(["save", "plan"]);
    // Not a link: no warning.
    expect(q(".link-warning").hidden).toBe(true);
  });

  it("labels unknown files as plain text", async () => {
    await mount({ file_read: () => ({ relPath: "LICENSE", content: "MIT", isModule: false, linkTarget: null }) }, "/repo/LICENSE");
    expect(q(".file-lang").textContent).toBe("Plain text");
    // Not in a module folder: nothing to plan.
    expect(q(".run-actions").hidden).toBe(true);
  });

  it("saves with Save and ⌘S, guarding against changes on disk", async () => {
    await mount();
    type("b = 2\n");
    expect(status().textContent).toBe("Unsaved changes");
    expect(status().className).toBe("file-status dirty");
    expect(saveBtn().disabled).toBe(false);

    saveBtn().click();
    expect(status().textContent).toBe("Saving…");
    expect(saveBtn().disabled).toBe(true);
    await flush();
    expect(saves).toEqual([{ path: PATH, content: "a = 1\nb = 2\n", expected: "a = 1\n" }]);
    expect(status().textContent).toBe("");

    type("c = 3\n");
    // From the button, not the editor; ⌘ as on macOS.
    expect(key("S", { ctrlKey: false, metaKey: true }, saveBtn()).defaultPrevented).toBe(true);
    await flush();
    expect(saves[1]).toEqual({ path: PATH, content: "a = 1\nb = 2\nc = 3\n", expected: "a = 1\nb = 2\n" });

    // Nothing to save: no call.
    key("s");
    await flush();
    expect(saves).toHaveLength(2);
  });

  it("closes the window on ⌘W and leaves other shortcuts alone", async () => {
    await mount();
    expect(key("s", { shiftKey: true }).defaultPrevented).toBe(false);
    expect(key("s", { altKey: true }).defaultPrevented).toBe(false);
    expect(key("s", { ctrlKey: false }).defaultPrevented).toBe(false);
    expect(key("k").defaultPrevented).toBe(false);
    expect(tauri.win.close).not.toHaveBeenCalled();
    expect(key("w").defaultPrevented).toBe(true);
    expect(tauri.win.close).toHaveBeenCalledTimes(1);
  });

  it("closes without asking when nothing changed", async () => {
    await mount();
    expect(await requestClose()).toBe(false);
    expect(saves).toEqual([]);
    expect(tauri.ask).not.toHaveBeenCalled();
  });

  describe("with Save automatically on (default)", () => {
    it("saves when the window closes", async () => {
      await mount();
      type("x");
      expect(await requestClose()).toBe(false);
      expect(saves).toEqual([{ path: PATH, content: "a = 1\nx", expected: "a = 1\n" }]);
    });

    it("saves when the editor or the window loses focus, e.g. to run a plan", async () => {
      await mount();
      blurEditor();
      blurWindow();
      await flush();
      expect(saves).toEqual([]); // nothing to save

      type("x");
      blurEditor();
      await flush();
      expect(saves).toHaveLength(1);
      type("y");
      blurWindow();
      await flush();
      expect(saves).toHaveLength(2);
      expect(tauri.win.close).not.toHaveBeenCalled();
    });

    it("runs one save at a time", async () => {
      const gate = deferred<void>();
      await mount({
        file_save: async (args) => {
          saves.push(args!);
          await gate.promise;
        },
      });
      type("x");
      blurEditor();
      await flush();
      blurEditor(); // already saving: ignored
      type("y");
      key("s");
      const first = requestClose();
      expect(await requestClose()).toBe(true); // already closing: refused
      await flush();
      expect(saves).toHaveLength(1);
      gate.resolve();
      expect(await first).toBe(false);
      // The second save picked up "y" after the first finished.
      expect(saves.map((s) => s.content)).toEqual(["a = 1\nx", "a = 1\nxy"]);
      expect(saves[1].expected).toBe("a = 1\nx");
    });

    it("shows a failed save, doesn't retry on every blur, and asks before discarding on close", async () => {
      let fail = true;
      await mount({
        file_save: (args) => {
          saves.push(args!);
          if (fail) throw "permission denied";
        },
      });
      type("x");
      blurEditor();
      await flush();
      expect(error().hidden).toBe(false);
      expect(error().textContent).toBe("permission denied");
      blurWindow();
      await flush();
      expect(saves).toHaveLength(1);

      // Typing clears the error and allows another try.
      type("y");
      expect(error().hidden).toBe(true);
      blurEditor();
      await flush();
      expect(saves).toHaveLength(2);

      tauri.ask.mockResolvedValueOnce(false);
      expect(await requestClose()).toBe(true);
      expect(tauri.ask).toHaveBeenCalledWith("Your changes couldn't be saved. Close the editor and discard them?", {
        title: "main.tf",
        kind: "warning",
        okLabel: "Discard",
        cancelLabel: "Keep editing",
      });

      tauri.ask.mockResolvedValueOnce(true);
      expect(await requestClose()).toBe(false);

      fail = false;
      expect(await requestClose()).toBe(false);
      expect(error().hidden).toBe(true);
    });
  });

  describe("when the file changed on disk", () => {
    const conflicting = (after: () => void = () => {}) => ({
      file_save: (args?: Record<string, unknown>) => {
        saves.push(args!);
        if (args!.expected !== null) throw CONFLICT;
        after();
      },
    });

    it("overwrites if asked", async () => {
      await mount(conflicting());
      type("x");
      tauri.ask.mockResolvedValueOnce(true);
      expect(await requestClose()).toBe(false);
      expect(tauri.ask).toHaveBeenCalledWith("envs/prod/main.tf was changed on disk since you opened it.\n\nOverwrite it with your version?", {
        title: "File changed on disk",
        kind: "warning",
        okLabel: "Overwrite",
        cancelLabel: "Keep editing",
      });
      expect(saves.map((s) => s.expected)).toEqual(["a = 1\n", null]);
    });

    it("keeps editing if not", async () => {
      await mount(conflicting());
      type("x");
      tauri.ask.mockResolvedValueOnce(false);
      saveBtn().click();
      await flush();
      expect(error().textContent).toBe(CONFLICT);
      expect(saves).toHaveLength(1);
    });

    it("reports an overwrite that fails", async () => {
      await mount(
        conflicting(() => {
          throw "read-only";
        }),
      );
      type("x");
      tauri.ask.mockResolvedValueOnce(true);
      saveBtn().click();
      await flush();
      expect(error().textContent).toBe("read-only");
    });
  });

  describe("with Save automatically off", () => {
    beforeEach(() => updateSettings({ autoSave: false }));

    it("never saves on its own, and asks before discarding", async () => {
      await mount();
      expect(q(".hint").textContent).toBe("⌘S to save.");
      type("x");
      blurEditor();
      blurWindow();
      await flush();
      expect(saves).toEqual([]);

      tauri.ask.mockResolvedValueOnce(false);
      expect(await requestClose()).toBe(true);
      expect(tauri.ask).toHaveBeenCalledWith("Discard your unsaved changes?", {
        title: "main.tf",
        kind: "warning",
        okLabel: "Discard",
        cancelLabel: "Keep editing",
      });
      tauri.ask.mockResolvedValueOnce(true);
      expect(await requestClose()).toBe(false);
      expect(saves).toEqual([]);
    });
  });

  describe("when the main window closes", () => {
    it("saves, answers and closes", async () => {
      await mount();
      type("x");
      await mainAsksToClose();
      expect(saves).toHaveLength(1);
      expect(tauri.emit).toHaveBeenCalledWith("editor://closed/editor-1", true);
      expect(tauri.win.destroy).toHaveBeenCalled();
    });

    it("answers no if the user keeps editing", async () => {
      updateSettings({ autoSave: false });
      await mount();
      type("x");
      tauri.ask.mockResolvedValueOnce(false);
      await mainAsksToClose();
      expect(tauri.emit).toHaveBeenCalledWith("editor://closed/editor-1", false);
      expect(tauri.win.destroy).not.toHaveBeenCalled();
    });
  });

  describe("in a module folder", () => {
    const plan = () => q<HTMLButtonElement>("[data-action=plan]");

    it("saves, then plans the folder in the main window", async () => {
      updateSettings({ autoSave: false }); // Plan saves regardless
      await mount();
      expect(q(".run-actions").hidden).toBe(false);
      expect(plan().textContent).toBe("Plan");
      expect(plan().title).toBe("Save, then plan envs/prod");
      type("x");
      plan().click();
      await flush();
      expect(saves).toHaveLength(1);
      expect(tauri.emitTo).toHaveBeenCalledWith("main", "editor://plan", { dir: "/repo/envs/prod" });

      // Nothing to save: straight to the plan.
      plan().click();
      await flush();
      expect(saves).toHaveLength(1);
      expect(tauri.emitTo).toHaveBeenCalledTimes(2);
    });

    it("doesn't plan if the save fails", async () => {
      await mount({
        file_save: () => {
          throw "disk full";
        },
      });
      type("x");
      plan().click();
      await flush();
      expect(error().textContent).toBe("disk full");
      expect(tauri.emitTo).not.toHaveBeenCalled();
    });

    it("plans the root folder, and finds the folder of a Windows path", async () => {
      await mount({ file_read: () => ({ relPath: "main.tf", content: "", isModule: true, linkTarget: null }) }, "C:\\repo\\main.tf");
      expect(plan().title).toBe("Save, then plan the root folder");
      plan().click();
      await flush();
      expect(tauri.emitTo).toHaveBeenCalledWith("main", "editor://plan", { dir: "C:\\repo" });
    });

    it("plans in the workspace window it was opened from", async () => {
      await mount({}, PATH, "workspace-3");
      plan().click();
      await flush();
      expect(tauri.emitTo).toHaveBeenCalledWith("workspace-3", "editor://plan", { dir: "/repo/envs/prod" });
    });
  });

  describe("a symlinked file", () => {
    const linked = { file_read: () => ({ relPath: "envs\\prod\\providers.tf", content: "", isModule: true, linkTarget: "shared/providers.tf" }) };

    it("warns at the top that changes affect every place linking to it", async () => {
      await mount(linked, "/repo/envs/prod/providers.tf");
      const warning = q(".link-warning");
      expect(warning.hidden).toBe(false);
      expect(document.body.firstElementChild).toBe(warning);
      expect(warning.getAttribute("role")).toBe("note");
      expect(warning.querySelector("code")!.textContent).toBe("shared/providers.tf");
      expect(warning.textContent).toBe("Linked file: this is shared/providers.tf. Changes here affect every place that links to it.");
    });

    it("plans the folder it was opened from, not where it points", async () => {
      await mount(linked, "/repo/envs/prod/providers.tf");
      expect(q<HTMLButtonElement>("[data-action=plan]").title).toBe("Save, then plan envs/prod");
      q<HTMLButtonElement>("[data-action=plan]").click();
      await flush();
      expect(tauri.emitTo).toHaveBeenCalledWith("main", "editor://plan", { dir: "/repo/envs/prod" });
    });
  });
});
