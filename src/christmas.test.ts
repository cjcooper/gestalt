import { afterEach, describe, expect, it } from "vitest";
import { christmasGreeting, initChristmas, sleepsUntilChristmas } from "./christmas";

const on = (y: number, m: number, d: number) => new Date(y, m - 1, d, 15, 30);

describe("sleepsUntilChristmas", () => {
  it("counts the nights from November 1 to Christmas Eve", () => {
    expect(sleepsUntilChristmas(on(2026, 11, 1))).toBe(54);
    expect(sleepsUntilChristmas(on(2026, 11, 26))).toBe(29);
    expect(sleepsUntilChristmas(on(2026, 12, 24))).toBe(1);
  });

  it("has nothing to count on and after the day, or earlier in the year", () => {
    expect(sleepsUntilChristmas(on(2026, 12, 25))).toBeNull();
    expect(sleepsUntilChristmas(on(2026, 12, 31))).toBeNull();
    expect(sleepsUntilChristmas(on(2027, 1, 6))).toBeNull();
    expect(sleepsUntilChristmas(on(2026, 10, 31))).toBeNull();
  });

  it("isn't thrown by a clock change", () => {
    // US daylight saving ends on November 1 2026; the count is in calendar days.
    expect(sleepsUntilChristmas(new Date(2026, 10, 1, 0, 0))).toBe(54);
    expect(sleepsUntilChristmas(new Date(2026, 10, 2, 23, 59))).toBe(53);
  });
});

describe("christmasGreeting", () => {
  it("counts down, then wishes a merry Christmas", () => {
    expect(christmasGreeting(on(2026, 11, 26))).toBe("🎄 29 sleeps until Christmas");
    expect(christmasGreeting(on(2026, 12, 24))).toBe("🎄 One more sleep until Christmas!");
    expect(christmasGreeting(on(2026, 12, 25))).toBe("🎄 Merry Christmas!");
  });

  it("is still festive out of season", () => {
    expect(christmasGreeting(on(2026, 12, 26))).toBe("🎄 Happy holidays from Gestalt");
    expect(christmasGreeting(on(2027, 7, 4))).toBe("🎄 Happy holidays from Gestalt");
  });
});

describe("initChristmas", () => {
  afterEach(() => {
    document.documentElement.removeAttribute("data-christmas");
    document.body.innerHTML = "";
  });

  it("dresses the page up and greets on the welcome panel", () => {
    document.body.innerHTML = `<p id="welcome-greeting"></p>`;
    initChristmas(on(2026, 12, 20));
    expect(document.documentElement.hasAttribute("data-christmas")).toBe(true);
    expect(document.getElementById("welcome-greeting")!.textContent).toBe("🎄 5 sleeps until Christmas");
  });

  it("does without a welcome panel, on today's date by default", () => {
    initChristmas();
    expect(document.documentElement.hasAttribute("data-christmas")).toBe(true);
  });
});
