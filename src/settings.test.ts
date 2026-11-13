import { beforeEach, describe, expect, it, vi } from "vitest";
import { getSettings, openSettings, updateSettings } from "./settings";

const dialog = () => document.querySelector<HTMLDialogElement>("dialog.settings");
const checkbox = () => dialog()!.querySelector<HTMLInputElement>("input[name=autoSave]")!;

beforeEach(() => {
  localStorage.clear();
  document.body.replaceChildren();
});

describe("settings storage", () => {
  it("defaults to saving automatically", () => {
    expect(getSettings()).toEqual({ autoSave: true });
  });

  it("stores and reads back changes", () => {
    updateSettings({ autoSave: false });
    expect(getSettings()).toEqual({ autoSave: false });
    expect(JSON.parse(localStorage.getItem("gestalt.settings")!)).toEqual({ autoSave: false });
    updateSettings({});
    expect(getSettings().autoSave).toBe(false);
  });

  it("ignores corrupt or wrongly typed values", () => {
    localStorage.setItem("gestalt.settings", "{not json");
    expect(getSettings()).toEqual({ autoSave: true });
    localStorage.setItem("gestalt.settings", '{"autoSave":"no"}');
    expect(getSettings()).toEqual({ autoSave: true });
    localStorage.setItem("gestalt.settings", "42");
    expect(getSettings()).toEqual({ autoSave: true });
  });

  it("survives storage being unavailable", () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    expect(getSettings()).toEqual({ autoSave: true });
    expect(() => updateSettings({ autoSave: false })).not.toThrow();
  });
});

describe("openSettings", () => {
  it("shows the current value and saves changes immediately", () => {
    updateSettings({ autoSave: false });
    openSettings();
    expect(dialog()!.open).toBe(true);
    expect(checkbox().checked).toBe(false);
    expect(document.activeElement).toBe(checkbox());

    checkbox().click();
    expect(getSettings().autoSave).toBe(true);
    dialog()!.querySelector<HTMLButtonElement>("button.primary")!.click();
    expect(dialog()).toBeNull();
  });

  it("opens only once, focusing the existing window", () => {
    openSettings();
    checkbox().blur();
    openSettings();
    expect(document.querySelectorAll("dialog.settings")).toHaveLength(1);
    expect(document.activeElement).toBe(checkbox());
  });

  it("closes on Esc or a click on the backdrop, not inside", () => {
    openSettings();
    const cancel = new Event("cancel", { cancelable: true });
    dialog()!.dispatchEvent(cancel);
    expect(cancel.defaultPrevented).toBe(true);
    expect(dialog()).toBeNull();

    openSettings();
    dialog()!.querySelector("section")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(dialog()).not.toBeNull();
    dialog()!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(dialog()).toBeNull();
  });
});
