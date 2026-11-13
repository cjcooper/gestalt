import { beforeEach, describe, expect, it, vi } from "vitest";

const m = vi.hoisted(() => ({
  listeners: new Map<string, (e: { payload: unknown }) => void>(),
  emitTo: vi.fn(),
  unlisten: vi.fn(),
  windows: [] as { label: string; destroy: ReturnType<typeof vi.fn> }[],
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (event: string, cb: (e: { payload: unknown }) => void) => {
    m.listeners.set(event, cb);
    return m.unlisten;
  },
  emitTo: m.emitTo,
}));
vi.mock("@tauri-apps/api/webviewWindow", () => ({ getAllWebviewWindows: async () => m.windows }));

import { REPLY_TIMEOUT_MS, closeEditorWindows, closeFrontDialog, closedEvent } from "./windows";

const win = (label: string) => ({ label, destroy: vi.fn() });

beforeEach(() => {
  m.listeners.clear();
  m.emitTo.mockReset();
  m.unlisten.mockReset();
  m.windows = [];
  document.body.replaceChildren();
});

describe("closeFrontDialog", () => {
  const dialog = (handlesEsc: boolean) => {
    const d = document.createElement("dialog");
    if (handlesEsc) d.addEventListener("cancel", (ev) => ev.preventDefault());
    document.body.append(d);
    d.showModal();
    return d;
  };

  it("does nothing without an open dialog", () => {
    document.body.append(document.createElement("dialog"));
    expect(closeFrontDialog()).toBe(false);
  });

  it("sends the front dialog an Esc, closing it if it doesn't handle one", () => {
    const back = dialog(false);
    const front = dialog(true);
    expect(closeFrontDialog()).toBe(true);
    expect(front.open).toBe(true); // handled it itself
    expect(back.open).toBe(true);
    front.close();
    expect(closeFrontDialog()).toBe(true);
    expect(back.open).toBe(false);
  });
});

describe("closeEditorWindows", () => {
  /** Answer each close request with `answers` in turn. */
  function editorsAnswer(...answers: boolean[]) {
    m.emitTo.mockImplementation(async (label: string, event: string) => {
      expect(event).toBe("editor://close");
      m.listeners.get(closedEvent(label))!({ payload: answers.shift() });
    });
  }

  it("asks each of the window's editor windows in turn, ignoring other windows", async () => {
    m.windows = [
      win("main"),
      win("editor-main-a"),
      win("about"),
      win("editor-workspace-1-c"),
      win("editor-workspace-12-d"),
      win("editor-main-b"),
    ];
    editorsAnswer(true, true);
    await expect(closeEditorWindows("main")).resolves.toBe(true);
    expect(m.emitTo.mock.calls.map((c) => c[0])).toEqual(["editor-main-a", "editor-main-b"]);
    m.emitTo.mockClear();
    editorsAnswer(true);
    await expect(closeEditorWindows("workspace-1")).resolves.toBe(true);
    expect(m.emitTo.mock.calls.map((c) => c[0])).toEqual(["editor-workspace-1-c"]);
    expect(m.unlisten).toHaveBeenCalledTimes(3);
  });

  it("stops at the first editor that stays open", async () => {
    m.windows = [win("editor-main-a"), win("editor-main-b")];
    editorsAnswer(false);
    await expect(closeEditorWindows("main")).resolves.toBe(false);
    expect(m.emitTo).toHaveBeenCalledTimes(1);
  });

  it("is true with no editors", async () => {
    await expect(closeEditorWindows("main")).resolves.toBe(true);
  });

  it("closes an editor that doesn't answer (still loading)", async () => {
    vi.useFakeTimers();
    const silent = win("editor-main-a");
    m.windows = [silent];
    const done = closeEditorWindows("main");
    await vi.advanceTimersByTimeAsync(REPLY_TIMEOUT_MS);
    await expect(done).resolves.toBe(true);
    expect(silent.destroy).toHaveBeenCalled();
    vi.useRealTimers();
  });
});
