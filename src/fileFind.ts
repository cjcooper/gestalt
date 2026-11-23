// The search field at the top of the Files tab, in two modes:
//
//  - Name or path (⌘P): type part of a name, or a path relative to the
//    opened folder. Matches list as you type, nearest first; Tab completes
//    the path a folder at a time, ↑/↓ choose, ⏎ opens a file in its editor
//    or shows a folder in the tree.
//  - Text in files (⇧⌘F): lines containing the text, grouped by file; ⏎ or
//    a click opens the file at that line.
//
// While there's a query the results replace the tree; Esc clears it.

import { invoke } from "@tauri-apps/api/core";
import { fileIcon, fileKind, icons } from "./icons";
import { completePath, matchPaths, normalizeQuery, type PathItem } from "./pathMatch";
import { highlightMatches } from "./textFind";

export type FindMode = "name" | "text";

/** What the finder needs from the window it's in. */
export interface FileFindHost {
  /** The opened folder, if any. */
  root(): string | null;
  /** Open a file in its editor, at `line` if given. */
  open(path: string, line?: number): void;
  /** Show a folder in the tree. */
  reveal(path: string): void;
  /** Bring the Files tab forward. */
  showFiles(): void;
}

export interface FileFind {
  /** Focus the field (switching mode if given), its text selected. */
  focus(mode?: FindMode): void;
  /** Files were added or removed: list them again. */
  invalidate(): void;
  /** Another folder was opened: start over. */
  reset(): void;
}

interface FileList {
  items: PathItem[];
  truncated: boolean;
}

interface TextMatch {
  rel: string;
  path: string;
  line: number;
  text: string;
}

interface SearchResults {
  matches: TextMatch[];
  filesSearched: number;
  truncated: boolean;
}

/** Most name matches listed. */
export const NAME_LIMIT = 200;
/** Typing pause before searching file contents. */
export const TEXT_DELAY_MS = 250;

const PLACEHOLDERS: Record<FindMode, string> = {
  name: "Go to file: name or path (⌘P)",
  text: "Find in files (⇧⌘F)",
};

const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

/** `text` with the characters at `hits` (offset by `from`) in <mark>s. */
function emphasise(text: string, hits: Set<number>, from: number): DocumentFragment {
  const frag = document.createDocumentFragment();
  let run = "";
  let marked = false;
  const flush = () => {
    if (!run) return;
    if (marked) {
      const m = document.createElement("mark");
      m.textContent = run;
      frag.append(m);
    } else {
      frag.append(run);
    }
    run = "";
  };
  [...text].forEach((ch, i) => {
    const hit = hits.has(from + i);
    if (hit !== marked) {
      flush();
      marked = hit;
    }
    run += ch;
  });
  flush();
  return frag;
}

const el = <K extends keyof HTMLElementTagNameMap>(tag: K, cls: string, ...kids: (Node | string)[]) => {
  const e = document.createElement(tag);
  e.className = cls;
  e.append(...kids);
  return e;
};

export function initFileFind(host: FileFindHost): FileFind {
  const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
  const input = $<HTMLInputElement>("file-find-input");
  const caseBtn = $<HTMLButtonElement>("file-find-case");
  const results = $("file-find-results");
  const status = $("file-find-status");
  const list = $("file-find-list");
  const tree = $("tree");
  const modeButtons = [...document.querySelectorAll<HTMLButtonElement>("[data-find-mode]")];

  let mode: FindMode = "name";
  let caseSensitive = false;
  let files: Promise<FileList> | null = null;
  /** Bumped per search, so a slow, superseded one doesn't win. */
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  /** The query + case the text results on show are for. */
  let shownText: string | null = null;
  /** The results that can be chosen; a name match also completes the path. */
  let rows: { el: HTMLElement; activate: () => void; complete?: string }[] = [];
  let selected = -1;

  function setMode(m: FindMode) {
    mode = m;
    for (const b of modeButtons) b.setAttribute("aria-checked", String(b.dataset.findMode === m));
    input.placeholder = PLACEHOLDERS[m];
    input.setAttribute("aria-label", PLACEHOLDERS[m]);
    caseBtn.hidden = m !== "text";
  }

  function select(i: number) {
    rows[selected]?.el.classList.remove("selected");
    selected = i;
    const row = rows[i];
    if (!row) return;
    row.el.classList.add("selected");
    row.el.scrollIntoView({ block: "nearest" });
  }

  function showResults(on: boolean) {
    results.hidden = !on;
    tree.hidden = on;
  }

  function setRows(next: typeof rows, items: HTMLElement[], message: string) {
    rows = next;
    selected = -1;
    list.replaceChildren(...items);
    status.textContent = message;
    select(0);
  }

  function clear() {
    generation++;
    clearTimeout(timer);
    shownText = null;
    input.value = "";
    setRows([], [], "");
    showResults(false);
  }

  const fileList = () => (files ??= invoke<FileList>("find_files", { root: host.root() }));

  async function searchNames() {
    const query = input.value;
    const gen = ++generation;
    let found: FileList;
    try {
      found = await fileList();
    } catch (e) {
      files = null; // try again next time
      if (gen === generation) setRows([], [], String(e));
      return;
    }
    if (gen !== generation) return;
    const matches = matchPaths(found.items, query, NAME_LIMIT);
    const items = matches.map(({ item, text, hits }) => {
      const nameAt = text.lastIndexOf("/", text.length - 2) + 1;
      const name = text.slice(nameAt);
      const set = new Set(hits);
      const li = el(
        "li",
        "find-row",
        el("span", "icon"),
        el("span", "find-name", emphasise(name, set, nameAt)),
        el("span", "find-dir", emphasise(text.slice(0, nameAt), set, 0)),
      );
      li.querySelector(".icon")!.innerHTML = item.isDir ? icons.folder : fileIcon(fileKind(name));
      li.title = item.path;
      li.setAttribute("role", "option");
      return li;
    });
    const next = matches.map(({ item, text }, i) => ({
      el: items[i],
      complete: text,
      activate: () => {
        if (!item.isDir) return host.open(item.path);
        clear();
        host.reveal(item.path);
      },
    }));
    const more = found.truncated ? ` (searched the first ${plural(found.items.length, "path")})` : "";
    const message = matches.length
      ? `${matches.length === NAME_LIMIT ? `First ${NAME_LIMIT}` : plural(matches.length, "match", "matches")} · Tab completes · ⏎ opens${more}`
      : `No file or folder matches${more}.`;
    setRows(next, items, message);
  }

  async function searchText() {
    clearTimeout(timer);
    const query = input.value;
    const key = `${caseSensitive}:${query}`;
    const gen = ++generation;
    shownText = key;
    status.textContent = "Searching…";
    let found: SearchResults;
    try {
      found = await invoke<SearchResults>("find_in_files", { root: host.root(), query, caseSensitive });
    } catch (e) {
      if (gen === generation) setRows([], [], String(e));
      return;
    }
    if (gen !== generation) return;
    const items: HTMLElement[] = [];
    const next: typeof rows = [];
    let file: string | null = null;
    for (const m of found.matches) {
      if (m.rel !== file) {
        file = m.rel;
        const count = found.matches.filter((x) => x.rel === m.rel).length;
        const name = m.rel.slice(m.rel.lastIndexOf("/") + 1);
        const head = el("li", "find-file", el("span", "icon"), el("span", "find-name", m.rel), el("span", "find-count", String(count)));
        head.querySelector(".icon")!.innerHTML = fileIcon(fileKind(name));
        head.title = m.path;
        items.push(head);
      }
      const text = el("span", "find-text", m.text.trim());
      highlightMatches(text, query, caseSensitive);
      const row = el("li", "find-row find-line", el("span", "find-line-no", String(m.line)), text);
      row.setAttribute("role", "option");
      items.push(row);
      next.push({ el: row, activate: () => host.open(m.path, m.line) });
    }
    const fileCount = new Set(found.matches.map((m) => m.rel)).size;
    const message = !found.matches.length
      ? `No matches in ${plural(found.filesSearched, "file")}.`
      : `${plural(found.matches.length, "line")} in ${plural(fileCount, "file")}` +
        (found.truncated ? ` (stopped at ${found.matches.length.toLocaleString()})` : "");
    setRows(next, items, message);
  }

  /** The query changed (or the mode, or case): show what matches now. */
  function update() {
    clearTimeout(timer);
    const query = mode === "name" ? normalizeQuery(input.value) : input.value;
    if (!query || !host.root()) return clear();
    showResults(true);
    if (mode === "name") {
      void searchNames();
      return;
    }
    // Contents are searched once typing pauses (or on ⏎).
    generation++;
    shownText = null;
    status.textContent = "Searching…";
    timer = setTimeout(() => void searchText(), TEXT_DELAY_MS);
  }

  input.addEventListener("input", update);
  input.addEventListener("keydown", (ev) => {
    const row = rows[selected];
    switch (ev.key) {
      case "ArrowDown":
      case "ArrowUp":
        ev.preventDefault();
        if (rows.length) select(Math.max(0, Math.min(rows.length - 1, selected + (ev.key === "ArrowDown" ? 1 : -1))));
        return;
      case "Enter":
        ev.preventDefault();
        if (mode === "text" && input.value && shownText !== `${caseSensitive}:${input.value}`) void searchText();
        else row?.activate();
        return;
      case "Tab":
        // Shift+Tab, and Tab in text mode, move focus as usual.
        if (mode !== "name" || ev.shiftKey || !row?.complete) return;
        ev.preventDefault();
        input.value = completePath(input.value, row.complete);
        update();
        return;
      case "Escape":
        ev.preventDefault();
        if (input.value) clear();
        else input.blur();
    }
  });
  list.addEventListener("click", (ev) => {
    const i = rows.findIndex((r) => r.el.contains(ev.target as Node));
    if (i < 0) return;
    select(i);
    rows[i].activate();
  });
  for (const b of modeButtons) {
    b.onclick = () => {
      setMode(b.dataset.findMode as FindMode);
      update();
      input.focus();
    };
  }
  caseBtn.onclick = () => {
    caseSensitive = !caseSensitive;
    caseBtn.setAttribute("aria-pressed", String(caseSensitive));
    update();
    input.focus();
  };
  setMode("name");

  return {
    focus(m) {
      host.showFiles();
      if (m && m !== mode) {
        setMode(m);
        update();
      }
      input.focus();
      input.select();
    },
    invalidate() {
      files = null;
      if (mode === "name" && !results.hidden) void searchNames();
    },
    reset() {
      files = null;
      clear();
    },
  };
}
