import { beforeEach, describe, expect, it, vi } from "vitest";
import { clearMatches, createTextFind, highlightMatches } from "./textFind";

const pre = (markup: string) => {
  const el = document.createElement("pre");
  el.innerHTML = markup;
  return el;
};
const marked = (el: Element) => [...el.querySelectorAll("mark.find-hit")].map((m) => m.textContent);

describe("highlightMatches", () => {
  it("marks every match, ignoring case unless asked", () => {
    const el = pre("Plan: 1 to add\nplan done\n");
    const groups = highlightMatches(el, "plan", false);
    expect(groups.map((g) => g.map((m) => m.textContent))).toEqual([["Plan"], ["plan"]]);
    expect(el.textContent).toBe("Plan: 1 to add\nplan done\n");
    clearMatches(el);
    expect(highlightMatches(el, "plan", true).length).toBe(1);
    expect(marked(el)).toEqual(["plan"]);
  });

  it("marks each piece of a match split across colours", () => {
    const el = pre('<span class="a">aws_s3_</span><span class="b">bucket.logs</span> and bucket');
    const groups = highlightMatches(el, "s3_bucket", false);
    expect(groups).toHaveLength(1);
    expect(groups[0].map((m) => m.textContent)).toEqual(["s3_", "bucket"]);
    expect(el.querySelector(".a")!.innerHTML).toBe('aws_<mark class="find-hit">s3_</mark>');
    expect(el.querySelector(".b")!.innerHTML).toBe('<mark class="find-hit">bucket</mark>.logs');
  });

  it("reads several roots, text nodes among them, as one text", () => {
    const el = pre("");
    const parts = [document.createTextNode("aws_s3_"), Object.assign(document.createElement("b"), { textContent: "bucket" })];
    el.append(...parts);
    const groups = highlightMatches(parts, "s3_bucket", false);
    expect(groups[0].map((m) => m.textContent)).toEqual(["s3_", "bucket"]);
    expect(el.innerHTML).toBe('aws_<mark class="find-hit">s3_</mark><b><mark class="find-hit">bucket</mark></b>');
  });

  it("handles neighbouring matches in one node, and no query", () => {
    const el = pre("aaaa");
    expect(highlightMatches(el, "aa", false)).toHaveLength(2);
    expect(el.innerHTML).toBe('<mark class="find-hit">aa</mark><mark class="find-hit">aa</mark>');
    expect(highlightMatches(pre("x"), "", false)).toEqual([]);
  });

  it("clears back to the original text nodes", () => {
    const el = pre('<span>one two</span> two');
    highlightMatches(el, "two", false);
    clearMatches(el);
    expect(el.innerHTML).toBe("<span>one two</span> two");
    expect(el.querySelector("span")!.childNodes).toHaveLength(1);
    clearMatches(el); // nothing to clear
    expect(el.innerHTML).toBe("<span>one two</span> two");
  });
});

describe("createTextFind", () => {
  let bar: HTMLElement;
  let out: HTMLElement;
  let find: ReturnType<typeof createTextFind>;
  const input = () => bar.querySelector<HTMLInputElement>(".find-input")!;
  const count = () => bar.querySelector(".find-count")!.textContent;
  const current = () => [...out.querySelectorAll("mark.current")].map((m) => m.textContent);
  const type = (s: string) => {
    input().value = s;
    input().dispatchEvent(new Event("input"));
  };
  const key = (k: string, extra: KeyboardEventInit = {}) => {
    const ev = new KeyboardEvent("keydown", { key: k, cancelable: true, ...extra });
    input().dispatchEvent(ev);
    return ev;
  };
  const click = (sel: string) => bar.querySelector<HTMLButtonElement>(sel)!.click();

  beforeEach(() => {
    document.body.innerHTML = '<div id="bar"></div><pre id="out">one Two\ntwo three\nTWO\n</pre>';
    bar = document.getElementById("bar")!;
    out = document.getElementById("out")!;
    find = createTextFind(bar, out);
  });

  it("starts hidden and opens focused, labelled", () => {
    expect(bar.hidden).toBe(true);
    expect(find.isOpen()).toBe(false);
    expect(bar.getAttribute("role")).toBe("search");
    find.open();
    expect(find.isOpen()).toBe(true);
    expect(document.activeElement).toBe(input());
    expect(input().placeholder).toBe("Find in output");
    expect(input().getAttribute("aria-label")).toBe("Find in output");
  });

  it("highlights as you type and goes to the first match", () => {
    find.open();
    type("two");
    expect(marked(out)).toEqual(["Two", "two", "TWO"]);
    expect(count()).toBe("1 of 3");
    expect(current()).toEqual(["Two"]);
    expect(Element.prototype.scrollIntoView).toHaveBeenCalled();
    type("zzz");
    expect(count()).toBe("No results");
    expect(bar.querySelector(".find-count")!.classList.contains("none")).toBe(true);
    type("");
    expect(count()).toBe("");
    expect(marked(out)).toEqual([]);
  });

  it("steps with Enter, Shift+Enter and the buttons, wrapping", () => {
    find.open();
    type("two");
    expect(key("Enter").defaultPrevented).toBe(true);
    expect(count()).toBe("2 of 3");
    key("Enter");
    key("Enter");
    expect(count()).toBe("1 of 3");
    key("Enter", { shiftKey: true });
    expect(count()).toBe("3 of 3");
    expect(current()).toEqual(["TWO"]);
    click(".find-prev");
    expect(count()).toBe("2 of 3");
    click(".find-next");
    expect(count()).toBe("3 of 3");
    find.step(true);
    expect(count()).toBe("1 of 3");
    expect(key("a").defaultPrevented).toBe(false);
  });

  it("matches case when asked", () => {
    find.open();
    type("Two");
    click(".find-case");
    expect(bar.querySelector(".find-case")!.getAttribute("aria-pressed")).toBe("true");
    expect(marked(out)).toEqual(["Two"]);
    expect(document.activeElement).toBe(input());
    click(".find-case");
    expect(marked(out)).toHaveLength(3);
  });

  it("closes with Escape or the button, clearing the marks", () => {
    find.open();
    type("two");
    expect(key("Escape").defaultPrevented).toBe(true);
    expect(bar.hidden).toBe(true);
    expect(marked(out)).toEqual([]);
    find.close(); // already closed
    // Reopening searches again for what's in the field, selected for retyping.
    find.open();
    expect(marked(out)).toHaveLength(3);
    expect(input().selectionStart).toBe(0);
    expect(input().selectionEnd).toBe(3);
    // Opening again while open keeps the current match.
    key("Enter");
    find.open();
    expect(count()).toBe("2 of 3");
    click(".find-close");
    expect(bar.hidden).toBe(true);
  });

  it("searches redrawn output without moving, then steps from either end", () => {
    find.open();
    type("two");
    out.innerHTML = "two\ntwo\n";
    find.refresh();
    expect(count()).toBe("2 matches");
    find.step(false);
    expect(count()).toBe("2 of 2");
    out.innerHTML = "two\n";
    find.refresh();
    expect(count()).toBe("1 match");
    find.step(true);
    expect(count()).toBe("1 of 1");
    out.innerHTML = "";
    find.refresh();
    find.step(true); // nothing to step to
    expect(count()).toBe("No results");
  });

  it("searches output as it arrives, while open with a query", () => {
    const line = (t: string) => {
      const span = document.createElement("span");
      span.textContent = t;
      out.append(span);
      find.appended([span]);
      return span;
    };
    line("two\n"); // closed: left alone
    expect(marked(out)).toEqual([]);
    find.open();
    line("two\n"); // no query yet
    expect(marked(out)).toEqual([]);
    type("two");
    expect(count()).toBe("1 of 5");
    expect(line("still two\n").querySelector("mark")).not.toBeNull();
    expect(count()).toBe("1 of 6");
  });

  it("takes its own label", () => {
    const other = document.createElement("div");
    createTextFind(other, out, "Find in file").open();
    expect(other.querySelector("input")!.placeholder).toBe("Find in file");
  });
});

beforeEach(() => {
  vi.spyOn(Element.prototype, "scrollIntoView").mockImplementation(() => {});
});
