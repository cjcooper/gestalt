import { describe, expect, it } from "vitest";
import { newAnsiState, renderAnsi, stripAnsi, type AnsiState } from "./ansi";

const ESC = "\x1b";

/** Render into a <div> so the result can be inspected as HTML. */
function render(line: string, state: AnsiState = newAnsiState()): HTMLDivElement {
  const div = document.createElement("div");
  div.append(renderAnsi(line, state));
  return div;
}

/** Final state after applying `seq` (an SGR parameter string). */
function sgr(params: string, state: AnsiState = newAnsiState()): AnsiState {
  renderAnsi(`${ESC}[${params}m`, state);
  return state;
}

describe("stripAnsi", () => {
  it("removes CSI, OSC and lone escapes but keeps text", () => {
    expect(stripAnsi(`${ESC}[1;32mPlan:${ESC}[0m ok`)).toBe("Plan: ok");
    expect(stripAnsi(`${ESC}[2Kcleared`)).toBe("cleared");
    expect(stripAnsi(`${ESC}]0;title\x07after`)).toBe("after");
    expect(stripAnsi(`${ESC}]8;;http://x${ESC}\\link`)).toBe("link");
    expect(stripAnsi(`${ESC}Mrev`)).toBe("rev");
    expect(stripAnsi("plain")).toBe("plain");
  });
});

describe("newAnsiState", () => {
  it("is plain and fresh each time", () => {
    const a = newAnsiState();
    expect(a).toEqual({ bold: false, dim: false, italic: false, underline: false, inverse: false, fg: null, bg: null });
    expect(newAnsiState()).not.toBe(a);
  });
});

describe("renderAnsi", () => {
  it("renders unstyled text as plain text nodes", () => {
    const div = render("hello");
    expect(div.innerHTML).toBe("hello");
    expect(div.childNodes[0]).toBeInstanceOf(Text);
  });

  it("renders an empty line as nothing", () => {
    expect(render("").childNodes).toHaveLength(0);
  });

  it("styles text after an SGR and resets with 0 / empty params", () => {
    const div = render(`a${ESC}[1mb${ESC}[0mc${ESC}[1md${ESC}[me`);
    expect(div.innerHTML).toBe('a<span class="ansi-bold">b</span>c<span class="ansi-bold">d</span>e');
  });

  it("drops non-SGR escapes without changing style", () => {
    const state = newAnsiState();
    expect(render(`${ESC}[2Kx${ESC}[3Ay`, state).innerHTML).toBe("xy");
    expect(state).toEqual(newAnsiState());
  });

  it("handles escapes at the very start and end of the line", () => {
    const state = newAnsiState();
    const div = render(`${ESC}[4munder${ESC}[24m`, state);
    expect(div.innerHTML).toBe('<span class="ansi-underline">under</span>');
    expect(state.underline).toBe(false);
  });

  it("carries state across lines", () => {
    const state = newAnsiState();
    render(`${ESC}[31m`, state);
    expect(render("still red", state).innerHTML).toBe('<span class="ansi-fg-1">still red</span>');
  });

  it("sets and clears text attributes", () => {
    expect(sgr("1;2;3;4;7")).toMatchObject({ bold: true, dim: true, italic: true, underline: true, inverse: true });
    expect(sgr("22", sgr("1;2"))).toMatchObject({ bold: false, dim: false });
    expect(sgr("23", sgr("3")).italic).toBe(false);
    expect(sgr("24", sgr("4")).underline).toBe(false);
    expect(sgr("27", sgr("7")).inverse).toBe(false);
    // Empty parameters count as 0 (reset).
    expect(sgr("1;;4")).toMatchObject({ bold: false, underline: true });
    // Unknown codes are ignored.
    expect(sgr("5;8;99")).toEqual(newAnsiState());
  });

  it("maps basic and bright colours to palette indices", () => {
    expect(sgr("30").fg).toBe(0);
    expect(sgr("37").fg).toBe(7);
    expect(sgr("40").bg).toBe(0);
    expect(sgr("47").bg).toBe(7);
    expect(sgr("90").fg).toBe(8);
    expect(sgr("97").fg).toBe(15);
    expect(sgr("100").bg).toBe(8);
    expect(sgr("107").bg).toBe(15);
    expect(sgr("39", sgr("31")).fg).toBeNull();
    expect(sgr("49", sgr("41")).bg).toBeNull();
  });

  it("parses 256-colour codes", () => {
    expect(sgr("38;5;9").fg).toBe(9);
    expect(sgr("38;5;16").fg).toBe("rgb(0,0,0)");
    expect(sgr("38;5;196").fg).toBe("rgb(255,0,0)");
    expect(sgr("48;5;67").bg).toBe("rgb(95,135,175)");
    expect(sgr("38;5;232").fg).toBe("rgb(8,8,8)");
    expect(sgr("38;5;255").fg).toBe("rgb(238,238,238)");
    // Out of range or missing index: no colour, and parsing continues after it.
    expect(sgr("38;5;300;1")).toMatchObject({ fg: null, bold: true });
    expect(sgr("38;5", sgr("31")).fg).toBeNull();
    // Colon-separated form.
    expect(sgr("38:5:2").fg).toBe(2);
  });

  it("parses truecolor codes, clamping components", () => {
    expect(sgr("38;2;10;20;30").fg).toBe("rgb(10,20,30)");
    expect(sgr("48;2;300;20;30;1")).toMatchObject({ bg: "rgb(255,20,30)", bold: true });
  });

  it("documents current behaviour for a truncated truecolor code", () => {
    // Known bug: `slice` returns fewer than three items, so the `?? 0`
    // default never applies and the colour string is invalid CSS (the
    // browser then ignores it, leaving the default colour).
    expect(sgr("38;2;10").fg).toBe("rgb(10,undefined,undefined)");
    const span = render(`${ESC}[38;2;10mx`).firstElementChild as HTMLSpanElement;
    expect(span.style.color).toBe("");
  });

  it("ignores the rest of the sequence after an unknown extended colour", () => {
    expect(sgr("38;9;1", sgr("32"))).toMatchObject({ fg: null, bold: false });
  });

  it("applies palette and rgb colours to spans", () => {
    expect(render(`${ESC}[31;42mx`).innerHTML).toBe('<span class="ansi-fg-1 ansi-bg-2">x</span>');
    const span = render(`${ESC}[38;2;1;2;3;48;5;200mx`).firstElementChild as HTMLSpanElement;
    expect(span.className).toBe("");
    expect(span.style.color).toBe("rgb(1, 2, 3)");
    expect(span.style.backgroundColor).toBe("rgb(255, 0, 215)");
  });

  it("renders bold, dim, italic and underline classes without colours", () => {
    const span = render(`${ESC}[1;2;3;4mx`).firstElementChild as HTMLSpanElement;
    expect(span.className).toBe("ansi-bold ansi-dim ansi-italic ansi-underline");
    expect(span.getAttribute("style")).toBeNull();
  });

  it("swaps colours for inverse, using the pane colours for defaults", () => {
    const def = render(`${ESC}[7mx`).firstElementChild as HTMLSpanElement;
    expect(def.style.color).toBe("var(--output-bg)");
    expect(def.style.backgroundColor).toBe("var(--fg)");

    const swapped = render(`${ESC}[7;31;44mx`).firstElementChild as HTMLSpanElement;
    expect(swapped.className).toBe("ansi-fg-4 ansi-bg-1");
  });
});
