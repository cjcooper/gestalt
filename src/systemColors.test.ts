import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
}));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen: tauri.listen }));

import { initSystemColors } from "./systemColors";

const style = () => document.documentElement.style;
const props = () => ({
  accentLight: style().getPropertyValue("--os-accent-light"),
  accentDark: style().getPropertyValue("--os-accent-dark"),
  highlightLight: style().getPropertyValue("--os-highlight-light"),
  highlightDark: style().getPropertyValue("--os-highlight-dark"),
});

const flush = () => new Promise((r) => setTimeout(r));

describe("initSystemColors", () => {
  beforeEach(() => {
    tauri.invoke.mockReset();
    tauri.listen.mockReset();
  });
  afterEach(() => {
    document.documentElement.removeAttribute("style");
  });

  it("applies the colours the backend reports, then live updates", async () => {
    tauri.invoke.mockResolvedValue({
      light: { accent: "#0a84ff", highlight: "#b3d7ff" },
      dark: { accent: null, highlight: "#3f638b" },
    });
    initSystemColors();
    await flush();
    expect(tauri.invoke).toHaveBeenCalledWith("system_colors");
    expect(props()).toEqual({ accentLight: "#0a84ff", accentDark: "", highlightLight: "#b3d7ff", highlightDark: "#3f638b" });

    expect(tauri.listen).toHaveBeenCalledWith("system://colors", expect.any(Function));
    const onChange = tauri.listen.mock.calls[0][1];
    onChange({
      payload: { light: { accent: "#ff0000", highlight: "#ffcccc" }, dark: { accent: "#00ff00", highlight: "#003300" } },
    });
    expect(props()).toEqual({ accentLight: "#ff0000", accentDark: "#00ff00", highlightLight: "#ffcccc", highlightDark: "#003300" });

    // null (non-macOS / Multicolor) clears every override.
    onChange({ payload: null });
    expect(props()).toEqual({ accentLight: "", accentDark: "", highlightLight: "", highlightDark: "" });
  });

  it("logs a failed lookup and leaves the theme alone", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    tauri.invoke.mockRejectedValue("boom");
    initSystemColors();
    await flush();
    expect(error).toHaveBeenCalledWith("boom");
    expect(props()).toEqual({ accentLight: "", accentDark: "", highlightLight: "", highlightDark: "" });
  });
});
