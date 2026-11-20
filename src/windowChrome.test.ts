import { afterEach, describe, expect, it, vi } from "vitest";

const tauri = vi.hoisted(() => ({ invoke: vi.fn(), setEffects: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));
vi.mock("@tauri-apps/api/window", () => ({
  Effect: { Sidebar: "sidebar" },
  EffectState: { FollowsWindowActiveState: "followsWindowActiveState" },
  getCurrentWindow: () => ({ setEffects: tauri.setEffects }),
}));

import { initWindowChrome } from "./windowChrome";

const vibrancy = () => document.documentElement.hasAttribute("data-vibrancy");

describe("initWindowChrome", () => {
  afterEach(() => {
    document.documentElement.removeAttribute("data-vibrancy");
    tauri.invoke.mockReset();
    tauri.setEffects.mockReset();
  });

  it("shows the sidebar material, dimmed in inactive windows", async () => {
    tauri.invoke.mockResolvedValue(true);
    tauri.setEffects.mockResolvedValue(undefined);
    await initWindowChrome();
    expect(tauri.invoke).toHaveBeenCalledWith("window_vibrancy");
    expect(tauri.setEffects).toHaveBeenCalledWith({ effects: ["sidebar"], state: "followsWindowActiveState" });
    expect(vibrancy()).toBe(true);
  });

  it("keeps the page's own background where there's no material", async () => {
    tauri.invoke.mockResolvedValueOnce(false);
    await initWindowChrome();
    tauri.invoke.mockRejectedValueOnce("no command");
    await initWindowChrome();
    expect(tauri.setEffects).not.toHaveBeenCalled();
    expect(vibrancy()).toBe(false);
  });

  it("keeps it too when the material can't be set", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    tauri.invoke.mockResolvedValue(true);
    tauri.setEffects.mockRejectedValue("not allowed");
    await initWindowChrome();
    expect(error).toHaveBeenCalledWith("not allowed");
    expect(vibrancy()).toBe(false);
  });
});
