// Find in a run's output (⌘F): a bar above the output that highlights every
// match, steps through them (⏎ / ⇧⏎, ⌘G / ⇧⌘G) and says where you are.
// Matches are wrapped in <mark> elements inside the output's own nodes, so
// tofu's colours stay as they are; new output is searched as it arrives.

import { icons } from "./icons";

const HIT = "find-hit";
const CURRENT = "current";

/**
 * Wrap each match of `query` in the text in `roots` (elements or text nodes,
 * read in order as one text) in a `<mark class="find-hit">`. A match split
 * across nodes (a colour change inside it) gets one mark per piece. Returns
 * the marks of each match, in document order.
 */
export function highlightMatches(roots: Node | Node[], query: string, caseSensitive: boolean): HTMLElement[][] {
  if (!query) return [];
  const nodes: { node: Text; start: number }[] = [];
  let text = "";
  const add = (n: Node) => {
    nodes.push({ node: n as Text, start: text.length });
    text += n.nodeValue;
  };
  for (const root of Array.isArray(roots) ? roots : [roots]) {
    if (root.nodeType === Node.TEXT_NODE) add(root);
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    for (let n = walker.nextNode(); n; n = walker.nextNode()) add(n);
  }
  const fold = (s: string) => (caseSensitive ? s : s.toLowerCase());
  const hay = fold(text);
  const needle = fold(query);
  const spans: [number, number][] = [];
  for (let at = hay.indexOf(needle); at >= 0; at = hay.indexOf(needle, at + needle.length)) {
    spans.push([at, at + needle.length]);
  }

  // From the end, so splitting a node leaves the offsets before it valid.
  const groups: HTMLElement[][] = [];
  for (let i = spans.length - 1; i >= 0; i--) {
    const [s, e] = spans[i];
    const marks: HTMLElement[] = [];
    for (let j = nodes.length - 1; j >= 0; j--) {
      const { node, start } = nodes[j];
      const end = start + node.length;
      if (end <= s || start >= e) continue;
      const from = Math.max(s, start) - start;
      const to = Math.min(e, end) - start;
      if (to < node.length) node.splitText(to);
      const piece = from > 0 ? node.splitText(from) : node;
      const mark = document.createElement("mark");
      mark.className = HIT;
      piece.replaceWith(mark);
      mark.append(piece);
      marks.unshift(mark);
    }
    groups.unshift(marks);
  }
  return groups;
}

/** Undo {@link highlightMatches}: put the text back as it was. */
export function clearMatches(root: Element) {
  const marks = root.querySelectorAll(`mark.${HIT}`);
  if (!marks.length) return;
  marks.forEach((m) => m.replaceWith(...m.childNodes));
  root.normalize();
}

export interface TextFind {
  /** Show the bar and focus its field, text selected. */
  open(): void;
  close(): void;
  isOpen(): boolean;
  /** Step to the next (or previous) match, wrapping around. */
  step(forward: boolean): void;
  /** The output was redrawn (another run shown): search it again. */
  refresh(): void;
  /** `nodes` were added to the end of the output: search them too. */
  appended(nodes: Node[]): void;
}

/** A find bar in `bar` for the text in `target`. */
export function createTextFind(bar: HTMLElement, target: HTMLElement, label = "Find in output"): TextFind {
  bar.classList.add("find-bar");
  bar.setAttribute("role", "search");
  bar.hidden = true;
  bar.innerHTML = `
    <span class="icon find-icon">${icons.search}</span>
    <input type="text" class="find-input" spellcheck="false" autocomplete="off" />
    <span class="find-count" aria-live="polite"></span>
    <button type="button" class="find-case" title="Match case" aria-pressed="false">Aa</button>
    <button type="button" class="find-prev icon-only" title="Previous match (⇧⏎)" aria-label="Previous match">${icons.chevronUp}</button>
    <button type="button" class="find-next icon-only" title="Next match (⏎)" aria-label="Next match">${icons.chevronDown}</button>
    <button type="button" class="find-close icon-only" title="Close (Esc)" aria-label="Close find">${icons.x}</button>`;
  const q = <T extends HTMLElement>(sel: string) => bar.querySelector<T>(sel)!;
  const input = q<HTMLInputElement>(".find-input");
  input.placeholder = label;
  input.setAttribute("aria-label", label);
  const count = q(".find-count");
  const caseBtn = q<HTMLButtonElement>(".find-case");

  let matches: HTMLElement[][] = [];
  /** Index of the current match; -1 when none is picked yet. */
  let current = -1;
  let caseSensitive = false;

  function renderCount() {
    count.classList.toggle("none", !!input.value && !matches.length);
    if (!input.value) count.textContent = "";
    else if (!matches.length) count.textContent = "No results";
    else if (current < 0) count.textContent = `${matches.length} match${matches.length === 1 ? "" : "es"}`;
    else count.textContent = `${current + 1} of ${matches.length}`;
  }

  function show(i: number) {
    matches[current]?.forEach((m) => m.classList.remove(CURRENT));
    current = i;
    const marks = matches[i];
    marks.forEach((m) => m.classList.add(CURRENT));
    marks[0].scrollIntoView({ block: "center", inline: "nearest" });
    renderCount();
  }

  /** Search again from scratch; `jump` goes to the first match. */
  function search(jump: boolean) {
    clearMatches(target);
    matches = bar.hidden ? [] : highlightMatches(target, input.value, caseSensitive);
    current = -1;
    if (jump && matches.length) show(0);
    else renderCount();
  }

  function step(forward: boolean) {
    if (!matches.length) return;
    const n = matches.length;
    show(current < 0 ? (forward ? 0 : n - 1) : (current + (forward ? 1 : -1) + n) % n);
  }

  function close() {
    if (bar.hidden) return;
    bar.hidden = true;
    search(false);
  }

  input.addEventListener("input", () => search(true));
  input.addEventListener("keydown", (ev) => {
    if (ev.key === "Enter") {
      ev.preventDefault();
      step(!ev.shiftKey);
    } else if (ev.key === "Escape") {
      ev.preventDefault();
      close();
    }
  });
  caseBtn.onclick = () => {
    caseSensitive = !caseSensitive;
    caseBtn.setAttribute("aria-pressed", String(caseSensitive));
    search(true);
    input.focus();
  };
  q(".find-prev").onclick = () => step(false);
  q(".find-next").onclick = () => step(true);
  q(".find-close").onclick = close;

  return {
    open() {
      const reopening = bar.hidden;
      bar.hidden = false;
      input.focus();
      input.select();
      if (reopening) search(true);
    },
    close,
    isOpen: () => !bar.hidden,
    step,
    refresh: () => search(false),
    appended(nodes) {
      if (bar.hidden || !input.value) return;
      matches.push(...highlightMatches(nodes, input.value, caseSensitive));
      renderCount();
    },
  };
}
