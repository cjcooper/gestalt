import { beforeEach, describe, expect, it, vi } from "vitest";

const m = vi.hoisted(() => ({ mountFileEditor: vi.fn(), initSystemColors: vi.fn() }));
vi.mock("./fileEditor", () => ({ mountFileEditor: m.mountFileEditor }));
vi.mock("./systemColors", () => ({ initSystemColors: m.initSystemColors }));

beforeEach(() => {
  vi.resetModules();
  m.mountFileEditor.mockReset();
  m.initSystemColors.mockReset();
  delete window.__GESTALT_EDITOR__;
  delete document.documentElement.dataset.platform;
});

describe("editor window entry", () => {
  it("edits the file the backend named", async () => {
    window.__GESTALT_EDITOR__ = { path: "/repo/main.tf", owner: "workspace-2" };
    await import("./editorWindow");
    expect(m.initSystemColors).toHaveBeenCalled();
    expect(m.mountFileEditor).toHaveBeenCalledWith(document.body, "/repo/main.tf", "workspace-2");
    expect(document.documentElement.dataset.platform).toBe("other");
  });

  it("does nothing without one, and knows macOS", async () => {
    vi.spyOn(navigator, "userAgent", "get").mockReturnValue("Mozilla/5.0 (Macintosh)");
    await import("./editorWindow");
    expect(m.mountFileEditor).not.toHaveBeenCalled();
    expect(document.documentElement.dataset.platform).toBe("mac");
  });
});
