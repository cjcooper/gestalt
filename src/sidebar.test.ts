import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { byId, deferred, flush, invokeRouter, loadIndexHtml } from "./testing/dom";

const tauri = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: tauri.invoke }));

type Sidebar = typeof import("./sidebar");

interface ModuleChange {
  path: string;
  deleted: boolean;
  dependsOn: string[];
}

const commit = (n: number, extra: Record<string, unknown> = {}) => ({
  hash: `hash${n}`,
  shortHash: `h${n}`,
  parents: ["p"],
  isMerge: false,
  authorName: "Ada",
  authorEmail: "ada@example.com",
  date: new Date(Date.now() - 3 * 3600 * 1000).toISOString(),
  subject: `Commit ${n}`,
  refs: [],
  modules: [],
  ...extra,
});

const log = (extra: Record<string, unknown> = {}) => ({
  branch: "main",
  repoRoot: "/repo",
  commits: [commit(1)],
  hasMore: false,
  remoteHasChanges: false,
  uncommitted: null,
  ...extra,
});

let sb: Sidebar;
let plan: ReturnType<typeof vi.fn<(dir: string) => void>>;
let apply: ReturnType<typeof vi.fn<(dir: string) => void>>;
let handlers: Record<string, (args?: Record<string, unknown>) => unknown>;

async function setup(h: typeof handlers = {}) {
  handlers = { git_is_repo: () => true, git_log: () => log(), ...h };
  tauri.invoke.mockImplementation(invokeRouter(new Proxy({}, { get: (_, k: string) => handlers[k] })));
  sb = await import("./sidebar");
  plan = vi.fn<(dir: string) => void>();
  apply = vi.fn<(dir: string) => void>();
  sb.initSidebar(plan, apply);
}

/** Open a folder and wait for the commit list to load. */
async function openFolder() {
  await sb.resetCommits();
  await flush();
}

const tab = (name: string) => document.querySelector<HTMLButtonElement>(`[data-tab="${name}"]`)!;
const panel = (name: string) => document.querySelector<HTMLElement>(`[data-panel="${name}"]`)!;
const rows = () => [...byId("commit-list").querySelectorAll<HTMLLIElement>(":scope > li")];
const gitLogCalls = () => tauri.invoke.mock.calls.filter(([c]) => c === "git_log");

beforeEach(() => {
  vi.resetModules();
  tauri.invoke.mockReset();
  localStorage.clear();
  loadIndexHtml();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("splitter", () => {
  const sidebar = () => byId("sidebar");
  const splitter = () => byId("splitter");
  const mockRect = (rect: Partial<DOMRect>) =>
    vi.spyOn(sidebar(), "getBoundingClientRect").mockReturnValue({ left: 0, width: 0, ...rect } as DOMRect);

  it("starts at the default width", async () => {
    await setup();
    expect(sidebar().style.width).toBe("320px");
    expect(splitter().getAttribute("aria-valuenow")).toBe("320");
  });

  it("restores and clamps the saved width", async () => {
    localStorage.setItem("gestalt.sidebarWidth", "250");
    await setup();
    expect(sidebar().style.width).toBe("250px");

    vi.resetModules();
    localStorage.setItem("gestalt.sidebarWidth", "50");
    await setup();
    expect(sidebar().style.width).toBe("180px");

    vi.resetModules();
    localStorage.setItem("gestalt.sidebarWidth", "5000");
    await setup();
    expect(sidebar().style.width).toBe(`${window.innerWidth - 320}px`);
  });

  it("falls back to the default when storage is unavailable", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    const set = vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    await setup();
    expect(sidebar().style.width).toBe("320px");
    mockRect({ width: 320 });
    splitter().dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight" }));
    expect(sidebar().style.width).toBe("330px");
    expect(set).toHaveBeenCalled();
  });

  it("resizes by dragging and saves the result on release", async () => {
    await setup();
    mockRect({ left: 10, width: 320 });
    const capture = vi.spyOn(splitter(), "setPointerCapture");
    const release = vi.spyOn(splitter(), "releasePointerCapture");

    const down = new PointerEvent("pointerdown", { pointerId: 7, cancelable: true });
    splitter().dispatchEvent(down);
    expect(down.defaultPrevented).toBe(true);
    expect(capture).toHaveBeenCalledWith(7);
    expect(document.body.classList.contains("resizing")).toBe(true);

    splitter().dispatchEvent(new PointerEvent("pointermove", { clientX: 410 }));
    expect(sidebar().style.width).toBe("400px");
    expect(localStorage.getItem("gestalt.sidebarWidth")).toBeNull();

    mockRect({ left: 10, width: 400 });
    splitter().dispatchEvent(new PointerEvent("pointerup", { pointerId: 7 }));
    expect(release).toHaveBeenCalledWith(7);
    expect(document.body.classList.contains("resizing")).toBe(false);
    expect(localStorage.getItem("gestalt.sidebarWidth")).toBe("400");

    // Listeners are gone after release.
    splitter().dispatchEvent(new PointerEvent("pointermove", { clientX: 600 }));
    expect(sidebar().style.width).toBe("400px");
  });

  it("resets to the default on double-click", async () => {
    localStorage.setItem("gestalt.sidebarWidth", "500");
    await setup();
    splitter().dispatchEvent(new MouseEvent("dblclick"));
    expect(sidebar().style.width).toBe("320px");
    expect(localStorage.getItem("gestalt.sidebarWidth")).toBe("320");
  });

  it("resizes with arrow keys (shift for bigger steps)", async () => {
    await setup();
    mockRect({ width: 300 });
    const key = (key: string, shiftKey = false) => {
      const ev = new KeyboardEvent("keydown", { key, shiftKey, cancelable: true });
      splitter().dispatchEvent(ev);
      return ev;
    };
    expect(key("ArrowLeft").defaultPrevented).toBe(true);
    expect(sidebar().style.width).toBe("290px");
    key("ArrowRight", true);
    expect(sidebar().style.width).toBe("350px");
    expect(localStorage.getItem("gestalt.sidebarWidth")).toBe("350");
    expect(key("Enter").defaultPrevented).toBe(false);
    expect(sidebar().style.width).toBe("350px");
  });

  it("re-clamps on window resize unless hidden", async () => {
    await setup();
    const rect = mockRect({ width: 0 });
    window.dispatchEvent(new Event("resize"));
    expect(sidebar().style.width).toBe("320px");

    rect.mockReturnValue({ left: 0, width: 900 } as DOMRect);
    window.dispatchEvent(new Event("resize"));
    expect(sidebar().style.width).toBe(`${window.innerWidth - 320}px`);
    expect(localStorage.getItem("gestalt.sidebarWidth")).toBeNull();
  });
});

describe("tabs", () => {
  it("shows Files with Commits hidden until a repo is open", async () => {
    await setup();
    expect(tab("commits").hidden).toBe(true);
    expect(tab("files").classList.contains("active")).toBe(true);
    expect(tab("files").getAttribute("aria-selected")).toBe("true");
    expect(tab("commits").getAttribute("aria-selected")).toBe("false");
    expect(panel("files").hidden).toBe(false);
    expect(panel("commits").hidden).toBe(true);
    // Clicking Commits with no repo still shows Files and doesn't query git.
    tab("commits").click();
    expect(panel("files").hidden).toBe(false);
    expect(tauri.invoke).not.toHaveBeenCalled();
  });

  it("opens a repo on Commits and remembers the clicked tab across refreshes", async () => {
    await setup();
    await openFolder();
    expect(tab("commits").hidden).toBe(false);
    expect(panel("commits").hidden).toBe(false);
    expect(gitLogCalls()).toEqual([["git_log", { skip: 0, limit: 200 }]]);

    tab("files").click();
    expect(panel("files").hidden).toBe(false);
    await sb.refreshCommits();
    await flush();
    expect(panel("files").hidden).toBe(false);
    // Not visible, so not reloaded yet...
    expect(gitLogCalls()).toHaveLength(1);
    // ...until shown.
    tab("commits").click();
    await flush();
    expect(gitLogCalls()).toHaveLength(2);
    // Already loaded: switching back and forth doesn't reload.
    tab("files").click();
    tab("commits").click();
    await flush();
    expect(gitLogCalls()).toHaveLength(2);

    // A newly opened folder starts on Commits again.
    tab("files").click();
    await openFolder();
    expect(panel("commits").hidden).toBe(false);
  });

  it("falls back to Files when the folder isn't a repo (or the check fails)", async () => {
    await setup({ git_is_repo: () => false });
    await openFolder();
    expect(tab("commits").hidden).toBe(true);
    expect(panel("files").hidden).toBe(false);
    expect(gitLogCalls()).toHaveLength(0);

    handlers.git_is_repo = () => {
      throw new Error("no git");
    };
    await openFolder();
    expect(tab("commits").hidden).toBe(true);
    expect(panel("files").hidden).toBe(false);
  });
});

describe("commit list", () => {
  it("renders the branch, commits and their metadata", async () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(new Date("2026-01-10T12:00:00Z"));
    await setup({
      git_log: () =>
        log({
          remoteHasChanges: true,
          commits: [
            commit(1, { date: "2026-01-10T11:59:30Z", refs: ["HEAD -> main", "origin/main", "tag: v1"] }),
            commit(2, { date: "2026-01-10T11:59:00Z", refs: ["HEAD", "feature"] }),
            commit(3, { date: "2026-01-10T10:00:00Z", isMerge: true, parents: ["a", "b"] }),
            commit(4, { date: "2026-01-09T12:00:00Z" }),
            commit(5, { date: "2026-01-01T12:00:00Z" }),
            commit(6, { date: "2025-11-01T12:00:00Z" }),
            commit(7, { date: "2024-01-01T12:00:00Z" }),
          ],
        }),
    });
    await openFolder();

    const branch = byId("commits-branch");
    expect(branch.querySelector(".icon svg")).not.toBeNull();
    expect(branch.textContent).toBe("mainServer has changes");
    expect(branch.querySelector(".remote-changes")!.getAttribute("title")).toMatch(/aren't in your local copy/);

    const r = rows();
    expect(r.map((li) => li.querySelector(".subject")!.textContent)).toEqual([
      "Commit 1",
      "Commit 2",
      "Commit 3",
      "Commit 4",
      "Commit 5",
      "Commit 6",
      "Commit 7",
    ]);
    expect(r.map((li) => li.querySelector(".meta")!.textContent)).toEqual([
      "h1 · Ada · just now",
      "h2 · Ada · 1 minute ago",
      "h3 · Ada · 2 hours ago",
      "h4 · Ada · 1 day ago",
      "h5 · Ada · 1 week ago",
      "h6 · Ada · 2 months ago",
      "h7 · Ada · 2 years ago",
    ]);
    expect(r[0].title).toMatch(/^hash1\nAda <ada@example.com>\n/);
    expect(r[0].title).not.toMatch(/Merge/);
    expect(r[2].className).toBe("commit merge");
    expect(r[2].title).toMatch(/\nMerge of 2 parents$/);

    const badges = (li: HTMLElement) => [...li.querySelectorAll(".ref")].map((b) => [b.className, b.textContent]);
    expect(badges(r[0])).toEqual([
      ["ref head", "main"],
      ["ref remote", "origin/main"],
      ["ref tag", "v1"],
    ]);
    expect(badges(r[1])).toEqual([
      ["ref head", "HEAD"],
      ["ref local", "feature"],
    ]);
    expect(r[3].querySelector(".refs")).toBeNull();

    // A commit that changed no module folder says so.
    expect(r[0].querySelector(".nothing-to-run")!.textContent).toBe("Nothing to run · no OpenTofu folders changed");

    const status = byId("commits-status");
    expect(status.hidden).toBe(true);
    expect(byId<HTMLButtonElement>("commits-more").hidden).toBe(true);
  });

  it("shows detached HEAD and an empty-repo message", async () => {
    await setup({ git_log: () => log({ branch: null, commits: [] }) });
    await openFolder();
    expect(byId("commits-branch").textContent).toBe("detached HEAD");
    const status = byId("commits-status");
    expect(status.hidden).toBe(false);
    expect(status.textContent).toBe("No commits yet.");
    expect(status.className).toBe("commits-status muted");
  });

  it("shows a loading state while fetching", async () => {
    const d = deferred<unknown>();
    await setup({ git_log: () => d.promise });
    const more = byId<HTMLButtonElement>("commits-more");
    const p = sb.resetCommits();
    await flush();
    const status = byId("commits-status");
    expect(status.textContent).toBe("Loading…");
    expect(status.hidden).toBe(false);
    expect(more.disabled).toBe(true);
    d.resolve(log());
    await p;
    await flush();
    expect(more.disabled).toBe(false);
    expect(rows()).toHaveLength(1);
  });

  it("selects a commit on click and keeps the selection across refreshes", async () => {
    await setup({ git_log: () => log({ commits: [commit(1), commit(2)] }) });
    await openFolder();
    rows()[1].click();
    expect(rows().map((r) => r.classList.contains("selected"))).toEqual([false, true]);
    rows()[0].click();
    expect(rows().map((r) => r.classList.contains("selected"))).toEqual([true, false]);

    await sb.refreshCommits();
    await flush();
    expect(rows().map((r) => r.classList.contains("selected"))).toEqual([true, false]);

    // A new folder forgets the selection.
    await openFolder();
    expect(rows().map((r) => r.classList.contains("selected"))).toEqual([false, false]);
  });

  it("pages with Load more, and reloads everything shown on refresh", async () => {
    let page = 0;
    await setup({
      git_log: (args) => {
        page++;
        const skip = args!.skip as number;
        const limit = args!.limit as number;
        const n = page === 1 ? 200 : page === 2 ? 50 : limit;
        return log({ commits: Array.from({ length: n }, (_, i) => commit(skip + i)), hasMore: page === 1 });
      },
    });
    await openFolder();
    const more = byId<HTMLButtonElement>("commits-more");
    expect(rows()).toHaveLength(200);
    expect(more.hidden).toBe(false);

    more.click();
    await flush();
    expect(gitLogCalls()[1]).toEqual(["git_log", { skip: 200, limit: 200 }]);
    expect(rows()).toHaveLength(250);
    expect(more.hidden).toBe(true);

    await sb.refreshCommits();
    await flush();
    expect(gitLogCalls()[2]).toEqual(["git_log", { skip: 0, limit: 250 }]);
    expect(rows()).toHaveLength(250);
  });

  it("shows a load error once, and retries on refresh", async () => {
    await setup({
      git_log: () => {
        throw "fatal: bad object";
      },
    });
    await openFolder();
    const status = byId("commits-status");
    expect(status.textContent).toBe("fatal: bad object");
    expect(status.className).toBe("commits-status error");
    expect(status.hidden).toBe(false);
    expect(byId("commits-branch").textContent).toBe("");
    expect(byId("commits-more").hidden).toBe(true);
    expect(byId<HTMLButtonElement>("commits-more").disabled).toBe(false);

    // No retry on tab switches.
    tab("files").click();
    tab("commits").click();
    await flush();
    expect(gitLogCalls()).toHaveLength(1);

    handlers.git_log = () => log();
    await sb.refreshCommits();
    await flush();
    expect(rows()).toHaveLength(1);
    expect(status.hidden).toBe(true);
  });

  it("ignores a superseded load (success or failure)", async () => {
    const first = deferred<unknown>();
    const second = deferred<unknown>();
    const calls = [first, second];
    await setup({ git_log: () => calls.shift()!.promise });
    void sb.resetCommits();
    await flush();
    void sb.resetCommits();
    await flush();
    second.resolve(log({ commits: [commit(2)] }));
    await flush();
    first.resolve(log({ commits: [commit(1)] }));
    await flush();
    expect(rows().map((r) => r.querySelector(".subject")!.textContent)).toEqual(["Commit 2"]);

    const third = deferred<unknown>();
    const fourth = deferred<unknown>();
    calls.push(third, fourth);
    void sb.resetCommits();
    await flush();
    void sb.resetCommits();
    await flush();
    fourth.resolve(log({ commits: [commit(4)] }));
    await flush();
    third.reject("stale failure");
    await flush();
    expect(rows().map((r) => r.querySelector(".subject")!.textContent)).toEqual(["Commit 4"]);
    expect(byId("commits-status").hidden).toBe(true);
  });

  it("does nothing on refresh before a folder is open", async () => {
    await setup();
    await sb.refreshCommits();
    await sb.refreshUncommitted();
    expect(tauri.invoke).not.toHaveBeenCalled();
  });
});

describe("folder lists", () => {
  const mods: ModuleChange[] = [
    { path: "net", deleted: false, dependsOn: [] },
    { path: "app", deleted: false, dependsOn: ["net"] },
    { path: "old", deleted: true, dependsOn: [] },
  ];

  it("lists changed folders in run order with Plan / Apply buttons", async () => {
    await setup({ git_log: () => log({ repoRoot: "/repo/", commits: [commit(1, { modules: mods })] }) });
    await openFolder();
    const row = rows()[0];
    const toggle = row.querySelector<HTMLButtonElement>(".folders-toggle")!;
    const list = row.querySelector<HTMLOListElement>(".module-changes")!;
    expect(toggle.textContent).toBe("3 folders");
    expect(list.hidden).toBe(true);
    expect(toggle.getAttribute("aria-expanded")).toBe("false");
    expect(toggle.title).toBe("Show folders and their Plan / Apply buttons");

    toggle.click();
    expect(list.hidden).toBe(false);
    expect(toggle.classList.contains("open")).toBe(true);
    expect(toggle.getAttribute("aria-expanded")).toBe("true");
    expect(toggle.title).toBe("Hide folders");
    // The toggle doesn't select the commit.
    expect(row.classList.contains("selected")).toBe(false);

    const items = [...list.children] as HTMLLIElement[];
    expect(items.map((li) => li.querySelector(".step")!.textContent)).toEqual(["1", "2", "–"]);
    expect(items.map((li) => li.querySelector(".step")!.className)).toEqual(["step", "step has-deps", "step"]);
    expect(items.map((li) => li.className)).toEqual(["", "", "deleted"]);
    expect(items.map((li) => li.title)).toEqual(["net", "app\nRun after: net (reads their state)", "old — folder deleted"]);
    expect(items[2].querySelector(".deleted-tag")!.textContent).toBe("deleted");
    expect(items[2].querySelector(".folder-actions")).toBeNull();

    const planBtn = items[1].querySelector<HTMLButtonElement>(".plan-btn")!;
    const applyBtn = items[1].querySelector<HTMLButtonElement>(".apply-btn")!;
    expect(planBtn.textContent).toBe("Plan");
    expect(planBtn.title).toBe("tofu plan in /repo/app");
    expect(applyBtn.title).toBe("Plan, review, then apply in /repo/app");
    planBtn.click();
    expect(plan).toHaveBeenCalledWith("/repo/app");
    applyBtn.click();
    expect(apply).toHaveBeenCalledWith("/repo/app");
    expect(row.classList.contains("selected")).toBe(false);

    // Expansion survives a refresh...
    await sb.refreshCommits();
    await flush();
    expect(rows()[0].querySelector<HTMLOListElement>(".module-changes")!.hidden).toBe(false);
    // ...and collapsing works too.
    rows()[0].querySelector<HTMLButtonElement>(".folders-toggle")!.click();
    expect(rows()[0].querySelector<HTMLOListElement>(".module-changes")!.hidden).toBe(true);
  });

  it("maps the repo root folder and labels single / deleted-only lists", async () => {
    await setup({
      git_log: () =>
        log({
          commits: [
            commit(1, { modules: [{ path: ".", deleted: false, dependsOn: [] }] }),
            commit(2, { modules: [{ path: "gone", deleted: true, dependsOn: [] }] }),
            commit(3, { modules: [mods[2], { ...mods[2], path: "gone2" }] }),
          ],
        }),
    });
    await openFolder();
    const [r1, r2, r3] = rows();
    expect(r1.querySelector(".folders-toggle")!.textContent).toBe("1 folder");
    r1.querySelector<HTMLButtonElement>(".plan-btn")!.click();
    expect(plan).toHaveBeenCalledWith("/repo");
    expect(r2.querySelector(".folders-toggle")!.textContent).toBe("1 deleted folder · nothing to run");
    expect(r3.querySelector(".folders-toggle")!.textContent).toBe("2 deleted folders · nothing to run");
  });

  it("omits run buttons when the repo root is unknown", async () => {
    await setup({ git_log: () => log({ repoRoot: "", commits: [commit(1, { modules: [mods[0]] })] }) });
    await openFolder();
    expect(rows()[0].querySelector(".folder-actions")).toBeNull();
  });
});

describe("uncommitted changes", () => {
  it("pins an expanded uncommitted row above the commits", async () => {
    await setup({
      git_log: () => log({ uncommitted: { fileCount: 2, modules: [{ path: "net", deleted: false, dependsOn: [] }] } }),
    });
    await openFolder();
    const [u, c] = rows();
    expect(u.className).toBe("commit uncommitted dirty");
    expect(u.querySelector(".subject")!.textContent).toBe("Uncommitted changes");
    expect(u.querySelector(".meta")!.textContent).toBe("2 files changed");
    expect(u.querySelector(".icon svg")).not.toBeNull();
    expect(u.title).toMatch(/compared with HEAD/);
    expect(u.querySelector<HTMLOListElement>(".module-changes")!.hidden).toBe(false);
    expect(c.querySelector(".subject")!.textContent).toBe("Commit 1");
  });

  it("describes clean and module-less states", async () => {
    await setup({ git_log: () => log({ uncommitted: { fileCount: 0, modules: [] } }) });
    await openFolder();
    let u = rows()[0];
    expect(u.className).toBe("commit uncommitted");
    expect(u.querySelector(".meta")!.textContent).toBe("Working tree clean");
    expect(u.querySelector(".nothing-to-run")).toBeNull();
    expect(u.querySelector(".folders-toggle")).toBeNull();

    handlers.git_uncommitted = () => ({ fileCount: 1, modules: [] });
    await sb.refreshUncommitted();
    u = rows()[0];
    expect(u.querySelector(".meta")!.textContent).toBe("1 file changed");
    expect(u.querySelector(".nothing-to-run")).not.toBeNull();
    expect(rows()).toHaveLength(2);
  });

  it("adds the row on refresh if the list had none", async () => {
    await setup({ git_uncommitted: () => ({ fileCount: 3, modules: [] }) });
    await openFolder();
    expect(rows()).toHaveLength(1);
    await sb.refreshUncommitted();
    expect(rows()).toHaveLength(2);
    expect(rows()[0].querySelector(".meta")!.textContent).toBe("3 files changed");
  });

  it("defers to a full reload when Commits isn't showing or loaded", async () => {
    await setup({ git_uncommitted: () => ({ fileCount: 3, modules: [] }) });
    await openFolder();
    tab("files").click();
    await sb.refreshUncommitted();
    expect(tauri.invoke).not.toHaveBeenCalledWith("git_uncommitted");
    tab("commits").click();
    await flush();
    expect(gitLogCalls()).toHaveLength(2);
  });

  it("does nothing outside a repo", async () => {
    await setup({ git_is_repo: () => false });
    await openFolder();
    await sb.refreshUncommitted();
    expect(tauri.invoke).not.toHaveBeenCalledWith("git_uncommitted");
  });

  it("drops superseded results and ones overtaken by a full reload", async () => {
    const results: ReturnType<typeof deferred<unknown>>[] = [];
    await setup({
      git_uncommitted: () => {
        const d = deferred<unknown>();
        results.push(d);
        return d.promise;
      },
    });
    await openFolder();

    // Two quick refreshes: only the latest applies.
    const a = sb.refreshUncommitted();
    const b = sb.refreshUncommitted();
    await flush();
    results[1].resolve({ fileCount: 2, modules: [] });
    results[0].resolve({ fileCount: 1, modules: [] });
    await Promise.all([a, b]);
    expect(rows()[0].querySelector(".meta")!.textContent).toBe("2 files changed");

    // A full reload starting meanwhile wins.
    const c = sb.refreshUncommitted();
    await flush();
    await sb.refreshCommits();
    await flush();
    results[2].resolve({ fileCount: 9, modules: [] });
    await c;
    expect(rows().some((r) => r.querySelector(".meta")!.textContent === "9 files changed")).toBe(false);
  });

  it("logs a failed refresh", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    await setup({
      git_uncommitted: () => {
        throw "index locked";
      },
    });
    await openFolder();
    await sb.refreshUncommitted();
    expect(error).toHaveBeenCalledWith("index locked");
    expect(rows()).toHaveLength(1);
  });
});
