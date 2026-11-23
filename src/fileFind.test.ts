import { beforeEach, describe, expect, it, vi } from "vitest";
import { byId, deferred, flush, invokeRouter, loadIndexHtml } from "./testing/dom";

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));

import { initFileFind, NAME_LIMIT, TEXT_DELAY_MS, type FileFind, type FileFindHost } from "./fileFind";

type Handler = (args?: Record<string, unknown>) => unknown;

const item = (rel: string, isDir = false) => ({ rel, path: `/repo/${rel}`, isDir });
const ITEMS = [item("envs", true), item("README.md"), item("envs/prod", true), item("envs/prod/main.tf"), item("envs/prod/prod.tfvars")];

const match = (rel: string, line: number, text: string) => ({ rel, path: `/repo/${rel}`, line, text });

let handlers: Record<string, Handler>;
let host: { [K in keyof FileFindHost]: ReturnType<typeof vi.fn> };
let root: string | null;
let find: FileFind;

function setup(h: Record<string, Handler> = {}) {
  handlers = {
    find_files: () => ({ items: ITEMS, truncated: false }),
    find_in_files: () => ({ matches: [], filesSearched: 3, truncated: false }),
    ...h,
  };
  tauri.invoke.mockImplementation(invokeRouter(new Proxy({}, { get: (_, k: string) => handlers[k] })));
  find = initFileFind(host as unknown as FileFindHost);
}

const input = () => byId<HTMLInputElement>("file-find-input");
const results = () => byId("file-find-results");
const status = () => byId("file-find-status").textContent;
const rows = () => [...byId("file-find-list").querySelectorAll<HTMLElement>(".find-row")];
const rowTexts = () => rows().map((r) => r.querySelector(".find-name")!.textContent! + "|" + r.querySelector(".find-dir")!.textContent);
const selectedRow = () => rows().findIndex((r) => r.classList.contains("selected"));
const modeButton = (m: string) => document.querySelector<HTMLButtonElement>(`[data-find-mode="${m}"]`)!;
const caseBtn = () => byId<HTMLButtonElement>("file-find-case");
const calls = (cmd: string) => tauri.invoke.mock.calls.filter(([c]) => c === cmd).map(([, a]) => a);

async function type(s: string) {
  input().value = s;
  input().dispatchEvent(new Event("input"));
  await flush();
}
function key(k: string, extra: KeyboardEventInit = {}) {
  const ev = new KeyboardEvent("keydown", { key: k, cancelable: true, ...extra });
  input().dispatchEvent(ev);
  return ev;
}

beforeEach(() => {
  tauri.invoke.mockReset();
  loadIndexHtml();
  byId("workspace").hidden = false; // hidden elements can't take focus
  root = "/repo";
  host = { root: vi.fn(() => root), open: vi.fn(), reveal: vi.fn(), showFiles: vi.fn() };
});

describe("go to file by name or path", () => {
  it("starts in name mode with the tree showing", () => {
    setup();
    expect(input().placeholder).toBe("Go to file: name or path (⌘P)");
    expect(input().getAttribute("aria-label")).toBe("Go to file: name or path (⌘P)");
    expect(modeButton("name").getAttribute("aria-checked")).toBe("true");
    expect(modeButton("text").getAttribute("aria-checked")).toBe("false");
    expect(caseBtn().hidden).toBe(true);
    expect(results().hidden).toBe(true);
    expect(byId("tree").hidden).toBe(false);
  });

  it("lists matches in place of the tree, the first selected", async () => {
    setup();
    await type("prod");
    expect(calls("find_files")).toEqual([{ root: "/repo" }]);
    expect(results().hidden).toBe(false);
    expect(byId("tree").hidden).toBe(true);
    expect(rowTexts()).toEqual(["prod/|envs/", "prod.tfvars|envs/prod/", "main.tf|envs/prod/"]);
    expect(rows()[1].querySelector(".find-name mark")!.textContent).toBe("prod");
    expect(rows()[2].querySelector(".find-dir mark")!.textContent).toBe("prod");
    expect(rows()[0].querySelector(".icon svg")).not.toBeNull();
    expect(rows()[0].title).toBe("/repo/envs/prod");
    expect(rows()[0].getAttribute("role")).toBe("option");
    expect(selectedRow()).toBe(0);
    expect(status()).toBe("3 matches · Tab completes · ⏎ opens");

    // The list is fetched once, and reused as you type.
    await type("main");
    expect(rowTexts()).toEqual(["main.tf|envs/prod/"]);
    expect(status()).toBe("1 match · Tab completes · ⏎ opens");
    expect(calls("find_files")).toHaveLength(1);

    await type("zzz");
    expect(rows()).toEqual([]);
    expect(status()).toBe("No file or folder matches.");
  });

  it("chooses with the arrow keys and opens with Enter or a click", async () => {
    setup();
    await type("prod");
    expect(key("ArrowDown").defaultPrevented).toBe(true);
    key("ArrowDown");
    key("ArrowDown"); // stays on the last
    expect(selectedRow()).toBe(2);
    key("ArrowUp");
    expect(selectedRow()).toBe(1);
    expect(key("Enter").defaultPrevented).toBe(true);
    expect(host.open).toHaveBeenCalledWith("/repo/envs/prod/prod.tfvars");
    // The query stays, to open another.
    expect(input().value).toBe("prod");

    rows()[2].querySelector(".find-name")!.dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(selectedRow()).toBe(2);
    expect(host.open).toHaveBeenLastCalledWith("/repo/envs/prod/main.tf");
    // Not on a row: nothing.
    byId("file-find-list").dispatchEvent(new MouseEvent("click", { bubbles: true }));
    expect(host.open).toHaveBeenCalledTimes(2);
  });

  it("shows a chosen folder in the tree and clears the search", async () => {
    setup();
    await type("prod");
    key("Enter");
    expect(host.reveal).toHaveBeenCalledWith("/repo/envs/prod");
    expect(host.open).not.toHaveBeenCalled();
    expect(input().value).toBe("");
    expect(results().hidden).toBe(true);
    expect(byId("tree").hidden).toBe(false);
  });

  it("completes the path a folder at a time with Tab", async () => {
    setup();
    await type("en");
    expect(key("Tab").defaultPrevented).toBe(true);
    await flush();
    expect(input().value).toBe("envs/");
    expect(rowTexts()[0]).toBe("envs/|");
    key("ArrowDown");
    key("Tab");
    await flush();
    expect(input().value).toBe("envs/prod/");
    key("ArrowDown");
    key("ArrowDown");
    key("Tab");
    await flush();
    expect(input().value).toBe("envs/prod/prod.tfvars");
    // Shift+Tab moves focus as usual.
    expect(key("Tab", { shiftKey: true }).defaultPrevented).toBe(false);
    // Nothing to complete to.
    await type("zzz");
    expect(key("Tab").defaultPrevented).toBe(false);
    expect(key("Enter").defaultPrevented).toBe(true);
    expect(key("ArrowDown").defaultPrevented).toBe(true);
    expect(host.open).not.toHaveBeenCalled();
  });

  it("says when there are too many to list, or the tree was too big to list whole", async () => {
    const many = Array.from({ length: NAME_LIMIT + 5 }, (_, i) => item(`f${i}.tf`));
    setup({ find_files: () => ({ items: many, truncated: true }) });
    await type("tf");
    expect(rows()).toHaveLength(NAME_LIMIT);
    expect(status()).toBe(`First ${NAME_LIMIT} · Tab completes · ⏎ opens (searched the first ${many.length} paths)`);
    await type("zzz");
    expect(status()).toBe(`No file or folder matches (searched the first ${many.length} paths).`);
  });

  it("shows a listing error, and tries again next time", async () => {
    let fail = true;
    setup({
      find_files: () => {
        if (fail) throw "boom";
        return { items: ITEMS, truncated: false };
      },
    });
    await type("main");
    expect(status()).toBe("boom");
    fail = false;
    await type("main.tf");
    expect(rowTexts()).toEqual(["main.tf|envs/prod/"]);
  });

  it("ignores a superseded search", async () => {
    const slow = deferred<unknown>();
    setup({ find_files: () => slow.promise });
    input().value = "main";
    input().dispatchEvent(new Event("input"));
    input().value = "";
    input().dispatchEvent(new Event("input"));
    slow.resolve({ items: ITEMS, truncated: false });
    await flush();
    expect(rows()).toEqual([]);
    expect(results().hidden).toBe(true);

    const failing = deferred<unknown>();
    handlers.find_files = () => failing.promise;
    find.invalidate();
    input().value = "main";
    input().dispatchEvent(new Event("input"));
    input().value = "";
    input().dispatchEvent(new Event("input"));
    failing.reject("boom");
    await flush();
    expect(status()).toBe("");
  });

  it("lists again after files change, refreshing what's shown", async () => {
    setup();
    find.invalidate(); // nothing shown: nothing to do
    expect(calls("find_files")).toHaveLength(0);
    await type("main");
    handlers.find_files = () => ({ items: [...ITEMS, item("main.tf")], truncated: false });
    find.invalidate();
    await flush();
    expect(rowTexts()).toEqual(["main.tf|", "main.tf|envs/prod/"]);
  });

  it("clears with Escape, then gives up focus", async () => {
    setup();
    await type("main");
    input().focus();
    expect(key("Escape").defaultPrevented).toBe(true);
    expect(input().value).toBe("");
    expect(results().hidden).toBe(true);
    expect(document.activeElement).toBe(input());
    key("Escape");
    expect(document.activeElement).not.toBe(input());
    expect(key("x").defaultPrevented).toBe(false);
  });

  it("does nothing without a folder open", async () => {
    root = null;
    setup();
    await type("main");
    expect(results().hidden).toBe(true);
    expect(calls("find_files")).toEqual([]);
  });

  it("starts over for another folder", async () => {
    setup();
    await type("main");
    find.reset();
    expect(input().value).toBe("");
    expect(results().hidden).toBe(true);
    root = "/other";
    await type("main");
    expect(calls("find_files")).toEqual([{ root: "/repo" }, { root: "/other" }]);
  });
});

describe("find in files", () => {
  const found = {
    matches: [
      match("envs/prod/main.tf", 3, '  bucket = "logs"'),
      match("envs/prod/main.tf", 9, "# Bucket policy"),
      match("README.md", 1, "Buckets"),
    ],
    filesSearched: 4,
    truncated: false,
  };
  const textRows = () => rows().map((r) => `${r.querySelector(".find-line-no")!.textContent}:${r.querySelector(".find-text")!.textContent}`);
  const fileHeads = () => [...byId("file-find-list").querySelectorAll(".find-file")].map((h) => h.textContent);

  async function search(q: string) {
    input().value = q;
    input().dispatchEvent(new Event("input"));
    key("Enter");
    await flush();
  }

  it("switches mode from the buttons or focus(), keeping the query", async () => {
    setup();
    await type("bucket");
    modeButton("text").click();
    expect(modeButton("text").getAttribute("aria-checked")).toBe("true");
    expect(input().placeholder).toBe("Find in files (⇧⌘F)");
    expect(caseBtn().hidden).toBe(false);
    expect(document.activeElement).toBe(input());
    expect(status()).toBe("Searching…");
    find.focus("name");
    expect(host.showFiles).toHaveBeenCalled();
    expect(input().placeholder).toBe("Go to file: name or path (⌘P)");
    expect(input().selectionStart).toBe(0);
    expect(input().selectionEnd).toBe(6);
    find.focus("name"); // already
    find.focus();
    expect(modeButton("name").getAttribute("aria-checked")).toBe("true");
  });

  it("searches once typing pauses, and lists lines by file", async () => {
    setup({ find_in_files: () => found });
    find.focus("text");
    await type("bucket");
    expect(status()).toBe("Searching…");
    expect(calls("find_in_files")).toEqual([]);
    await new Promise((r) => setTimeout(r, TEXT_DELAY_MS + 20));
    await flush();
    expect(calls("find_in_files")).toEqual([{ root: "/repo", query: "bucket", caseSensitive: false }]);
    expect(fileHeads()).toEqual(["envs/prod/main.tf2", "README.md1"]);
    expect(textRows()).toEqual(['3:bucket = "logs"', "9:# Bucket policy", "1:Buckets"]);
    expect([...rows()[1].querySelectorAll("mark")].map((m) => m.textContent)).toEqual(["Bucket"]);
    expect(status()).toBe("3 lines in 2 files");
    expect(selectedRow()).toBe(0);

    // Enter opens the chosen line (the search is up to date).
    key("ArrowDown");
    key("Enter");
    expect(host.open).toHaveBeenCalledWith("/repo/envs/prod/main.tf", 9);
    rows()[2].click();
    expect(host.open).toHaveBeenLastCalledWith("/repo/README.md", 1);
    expect(calls("find_in_files")).toHaveLength(1);
  });

  it("searches right away on Enter, and again when case matters", async () => {
    setup({ find_in_files: () => found });
    find.focus("text");
    await search("bucket");
    expect(calls("find_in_files")).toHaveLength(1);
    caseBtn().click();
    expect(caseBtn().getAttribute("aria-pressed")).toBe("true");
    key("Enter");
    await flush();
    expect(calls("find_in_files")[1]).toEqual({ root: "/repo", query: "bucket", caseSensitive: true });
    expect(document.activeElement).toBe(input());
    // The pending timer was dropped: no third search.
    await new Promise((r) => setTimeout(r, TEXT_DELAY_MS + 20));
    expect(calls("find_in_files")).toHaveLength(2);
  });

  it("says when nothing matched, or it stopped early", async () => {
    setup();
    find.focus("text");
    await search("nope");
    expect(status()).toBe("No matches in 3 files.");
    handlers.find_in_files = () => ({ matches: [match("a.tf", 1, "x")], filesSearched: 1, truncated: true });
    await search("x");
    expect(status()).toBe("1 line in 1 file (stopped at 1)");
    expect(fileHeads()).toEqual(["a.tf1"]);
  });

  it("shows a search error, and ignores a superseded search", async () => {
    setup({
      find_in_files: () => {
        throw "bad";
      },
    });
    find.focus("text");
    await search("x");
    expect(status()).toBe("bad");

    const slow = deferred<unknown>();
    handlers.find_in_files = () => slow.promise;
    await search("y");
    await type("yz"); // a new search pending
    slow.resolve(found);
    await flush();
    expect(rows()).toEqual([]);
    expect(status()).toBe("Searching…");

    const failing = deferred<unknown>();
    handlers.find_in_files = () => failing.promise;
    await search("q");
    await type("");
    failing.reject("late");
    await flush();
    expect(status()).toBe("");
  });

  it("Enter with nothing typed does nothing", async () => {
    setup();
    find.focus("text");
    key("Enter");
    await flush();
    expect(calls("find_in_files")).toEqual([]);
    expect(host.open).not.toHaveBeenCalled();
  });
});
