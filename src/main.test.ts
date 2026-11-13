import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { byId, deferred, flush, invokeRouter, loadIndexHtml } from "./testing/dom";
import { icons } from "./icons";
import type { DirEntry } from "./runs";

// ---- Mocks -------------------------------------------------------------------

interface MenuItemOpts {
  text?: string;
  action?: () => void;
  predefined?: string;
}

const m = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn(),
  windowLabel: "main",
  setTitle: vi.fn(),
  onCloseRequested: vi.fn(),
  menus: [] as { items: { text?: string; action?: () => void; predefined?: string }[]; popup: () => Promise<void> }[],
  ask: vi.fn(),
  message: vi.fn(),
  open: vi.fn(),
  openCloudSettings: vi.fn(),
  openImportDialog: vi.fn(),
  initSystemColors: vi.fn(),
  initSidebar: vi.fn(),
  refreshCommits: vi.fn(),
  refreshUncommitted: vi.fn(),
  resetCommits: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: m.invoke }));
vi.mock("@tauri-apps/api/webviewWindow", () => ({
  getCurrentWebviewWindow: () => ({
    label: m.windowLabel,
    listen: m.listen,
    setTitle: m.setTitle,
    onCloseRequested: m.onCloseRequested,
  }),
}));
vi.mock("@tauri-apps/api/menu", () => ({
  Menu: {
    new: async (opts: { items: MenuItemOpts[] }) => {
      const menu = { items: opts.items, popup: vi.fn(async () => {}) };
      m.menus.push(menu);
      return menu;
    },
  },
  MenuItem: { new: async (opts: MenuItemOpts) => ({ ...opts }) },
  PredefinedMenuItem: { new: async (opts: { item: string }) => ({ predefined: opts.item }) },
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ ask: m.ask, message: m.message, open: m.open }));
vi.mock("./cloudSettings", async (actual) => ({
  ...(await actual<typeof import("./cloudSettings")>()),
  openCloudSettings: m.openCloudSettings,
}));
vi.mock("./importDialog", () => ({ openImportDialog: m.openImportDialog }));
vi.mock("./systemColors", () => ({ initSystemColors: m.initSystemColors }));
vi.mock("./sidebar", () => ({
  initSidebar: m.initSidebar,
  refreshCommits: m.refreshCommits,
  refreshUncommitted: m.refreshUncommitted,
  resetCommits: m.resetCommits,
}));

// ---- Fixtures ----------------------------------------------------------------

type Handler = (args?: Record<string, unknown>) => unknown;

const dir = (path: string, extra: Partial<DirEntry> = {}): DirEntry => ({
  name: path.split("/").pop()!,
  path,
  isDir: true,
  hasChildren: true,
  isModule: false,
  ...extra,
});
const file = (path: string): DirEntry => dir(path, { isDir: false, hasChildren: false });

/** A small infra repo: list_dir results by folder. */
const fs: Record<string, DirEntry[]> = {
  "/repo": [
    dir("/repo/envs"),
    dir("/repo/empty", { hasChildren: false }),
    dir("/repo/broken"),
    file("/repo/README.md"),
  ],
  "/repo/envs": [dir("/repo/envs/prod", { isModule: true }), file("/repo/envs/common.tfvars")],
  "/repo/envs/prod": [file("/repo/envs/prod/main.tf"), file("/repo/envs/prod/prod.tfvars")],
};

const PLAN_LINE = "Plan: 2 to add, 0 to change, 0 to destroy.";

let handlers: Record<string, Handler>;
const listeners = new Map<string, (e: { payload: unknown }) => void>();
let docListeners: [string, EventListenerOrEventListenerObject][] = [];
let closeRequested: ((e: { preventDefault: () => void }) => Promise<void>) | undefined;

function emit(event: string, payload: unknown = {}) {
  listeners.get(event)!({ payload });
}

interface BootOptions {
  handlers?: Record<string, Handler>;
  userAgent?: string;
  lastRoot?: string;
  beforeImport?: () => void;
}

/** Load index.html and import main.ts fresh, as the app does at startup. */
async function boot(opts: BootOptions = {}) {
  handlers = {
    shell_env_status: () => ({ shell: "/bin/zsh", varCount: 42, error: null }),
    tofu_version: () => "OpenTofu v1.8.0",
    initial_root: () => null,
    open_root: (a) => dir(a!.path as string, { name: a!.path as string }),
    list_dir: (a) => {
      if (a!.path === "/repo/broken") throw new Error("permission denied");
      return fs[a!.path as string] ?? [];
    },
    run_plan: () => 1,
    cloud_effective: () => [],
    cloud_folders: () => ({}),
    ...opts.handlers,
  };
  m.invoke.mockImplementation(invokeRouter(new Proxy({}, { get: (_, k: string) => handlers[k] })));
  if (opts.userAgent) vi.spyOn(navigator, "userAgent", "get").mockReturnValue(opts.userAgent);
  if (opts.lastRoot) localStorage.setItem("gestalt.lastRoot", opts.lastRoot);
  opts.beforeImport?.();
  await import("./main");
  await flush();
}

/** Boot with /repo open. */
const bootRepo = (opts: BootOptions = {}) => boot({ lastRoot: "/repo", ...opts });

/** The Plan / Apply callbacks main.ts gave the sidebar (latest instance). */
const plan = (d: string) => (m.initSidebar.mock.lastCall![0] as (d: string) => void)(d);
const applyPlan = (d: string) => (m.initSidebar.mock.lastCall![1] as (d: string) => void)(d);

/** Markup as the DOM serialises it (e.g. `<path/>` becomes `<path></path>`). */
function html(markup: string) {
  const div = document.createElement("div");
  div.innerHTML = markup;
  return div.innerHTML;
}
const invoked = (cmd: string) => m.invoke.mock.calls.filter(([c]) => c === cmd).map(([, a]) => a);

const tabs = () => [...byId("run-tabs").children] as HTMLElement[];
const tabLabels = () => tabs().map((t) => t.querySelector(".run-tab-label")!.textContent);
const selectedTab = () => tabs().findIndex((t) => t.classList.contains("selected"));
const closeButton = (i: number) => tabs()[i].querySelector<HTMLButtonElement>(".run-tab-close")!;
const output = () => byId("run-output");
const status = () => byId("run-status").textContent;

const out = (runId: number, line: string, stream = "stdout") => emit("tofu://output", { runId, stream, line });
const exitEv = (runId: number, extra: Record<string, unknown> = {}) =>
  emit("tofu://exit", {
    runId,
    code: 0,
    success: true,
    cancelled: false,
    hasChanges: null,
    error: null,
    started: true,
    ...extra,
  });

const row = (label: string) =>
  [...document.querySelectorAll<HTMLElement>("#tree .tree-row")].find((r) => r.querySelector(".label")!.textContent === label)!;
const treeLabels = () => [...document.querySelectorAll("#tree .tree-row")]
  .filter((r) => !r.closest("ul[hidden]"))
  .map((r) => r.querySelector(".label")!.textContent);

function rightClick(el: HTMLElement) {
  const ev = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
  el.dispatchEvent(ev);
  return ev;
}
const lastMenu = () => m.menus[m.menus.length - 1];
const menuTexts = () => lastMenu().items.map((i) => i.text ?? `--${i.predefined}`);
const menuAction = (text: string) => lastMenu().items.find((i) => i.text === text)!.action!();

function key(k: string, mods: KeyboardEventInit = {}) {
  const ev = new KeyboardEvent("keydown", { key: k, cancelable: true, bubbles: true, ...mods });
  document.dispatchEvent(ev);
  return ev;
}

beforeEach(() => {
  vi.resetModules();
  vi.resetAllMocks();
  m.menus.length = 0;
  listeners.clear();
  m.listen.mockImplementation(async (event: string, cb: (e: { payload: unknown }) => void) => {
    listeners.set(event, cb);
    return () => {};
  });
  m.openImportDialog.mockResolvedValue(null);
  m.openCloudSettings.mockResolvedValue(false);
  m.windowLabel = "main";
  m.setTitle.mockResolvedValue(undefined);
  closeRequested = undefined;
  m.onCloseRequested.mockImplementation(async (cb: typeof closeRequested) => {
    closeRequested = cb;
    return () => {};
  });
  m.resetCommits.mockResolvedValue(undefined);
  m.refreshCommits.mockResolvedValue(undefined);
  m.refreshUncommitted.mockResolvedValue(undefined);
  localStorage.clear();
  loadIndexHtml();
  // Track document listeners so each fresh main.ts instance can be torn down.
  docListeners = [];
  const add = document.addEventListener.bind(document);
  vi.spyOn(document, "addEventListener").mockImplementation((type: string, l: EventListenerOrEventListenerObject) => {
    docListeners.push([type, l]);
    add(type, l);
  });
});

afterEach(() => {
  for (const [type, l] of docListeners) document.removeEventListener(type, l);
  vi.useRealTimers();
});

// ---- Tests -------------------------------------------------------------------

describe("startup", () => {
  it("fills icons, env status and tofu version, and shows the welcome panel", async () => {
    await boot({
      beforeImport: () => {
        const unknown = document.createElement("span");
        unknown.dataset.icon = "nope";
        unknown.id = "unknown-icon";
        unknown.textContent = "placeholder";
        document.body.append(unknown);
      },
    });
    expect(document.documentElement.dataset.platform).toBe("other");
    expect(document.querySelector("#open-root .icon")!.innerHTML).toBe(html(icons.folderPlus));
    expect(document.querySelector("#refresh-tree .icon")!.innerHTML).toBe(html(icons.refresh));
    expect(byId("unknown-icon").innerHTML).toBe("");

    const env = byId("env-status");
    expect(env.textContent).toBe("env: zsh (42 vars)");
    expect(env.title).toMatch(/login shell \(\/bin\/zsh\)/);
    expect(env.classList.contains("error")).toBe(false);
    expect(byId("tofu-version").textContent).toBe("OpenTofu v1.8.0");

    expect(byId("welcome").hidden).toBe(false);
    expect(byId("workspace").hidden).toBe(true);
    expect(byId("welcome-error").hidden).toBe(true);
    expect(m.initSystemColors).toHaveBeenCalled();
    expect(m.initSidebar).toHaveBeenCalledWith(expect.any(Function), expect.any(Function));
    expect(byId("run-header").hidden).toBe(true);
  });

  it("marks the platform on macOS", async () => {
    await boot({ userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0)" });
    expect(document.documentElement.dataset.platform).toBe("mac");
  });

  it("reports env and tofu problems", async () => {
    await boot({
      handlers: {
        shell_env_status: () => ({ shell: null, varCount: 0, error: "zsh: timed out" }),
        tofu_version: () => {
          throw "tofu: not on PATH";
        },
      },
    });
    const env = byId("env-status");
    expect(env.textContent).toBe("shell env not loaded");
    expect(env.title).toBe("zsh: timed out\n\ntofu will run with the app's default environment.");
    expect(env.classList.contains("error")).toBe(true);
    const tofu = byId("tofu-version");
    expect(tofu.textContent).toBe("tofu not found");
    expect(tofu.title).toBe("tofu: not on PATH");
    expect(tofu.classList.contains("error")).toBe(true);
  });

  it("names a generic shell when none is known, and logs a failed env lookup", async () => {
    await boot({ handlers: { shell_env_status: () => ({ shell: null, varCount: 3, error: null }) } });
    expect(byId("env-status").textContent).toBe("env: shell (3 vars)");

    vi.resetModules();
    loadIndexHtml();
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    await boot({
      handlers: {
        shell_env_status: () => {
          throw "no shell";
        },
      },
    });
    expect(error).toHaveBeenCalledWith("no shell");
    expect(byId("env-status").textContent).toBe("loading env…");
  });

  it("reloads the shell env and re-checks tofu", async () => {
    await boot({ handlers: { tofu_version: () => { throw "missing"; } } });
    const btn = byId<HTMLButtonElement>("env-reload");
    const d = deferred<unknown>();
    handlers.reload_shell_env = () => d.promise;
    handlers.tofu_version = () => "OpenTofu v1.9.0";
    btn.click();
    expect(btn.disabled).toBe(true);
    expect(byId("env-status").textContent).toBe("loading env…");
    d.resolve({ shell: "/usr/local/bin/fish", varCount: 7, error: null });
    await flush();
    expect(btn.disabled).toBe(false);
    expect(byId("env-status").textContent).toBe("env: fish (7 vars)");
    const tofu = byId("tofu-version");
    expect(tofu.textContent).toBe("OpenTofu v1.9.0");
    expect(tofu.title).toBe("");
    expect(tofu.classList.contains("error")).toBe(false);
  });

  it("re-enables Reload env when the reload fails (the error propagates)", async () => {
    await boot();
    handlers.reload_shell_env = () => {
      throw "login shell crashed";
    };
    const btn = byId<HTMLButtonElement>("env-reload");
    await expect((btn.onclick as () => Promise<void>)()).rejects.toBe("login shell crashed");
    expect(btn.disabled).toBe(false);
    // Known gap: the status keeps saying it's loading.
    expect(byId("env-status").textContent).toBe("loading env…");
  });
});

describe("opening a folder", () => {
  it("opens the configured root (command line / GESTALT_ROOT)", async () => {
    await boot({ lastRoot: "/old", handlers: { initial_root: () => ({ path: "/repo", source: "GESTALT_ROOT" }) } });
    expect(invoked("open_root")).toEqual([{ path: "/repo" }]);
    expect(byId("root-path").textContent).toBe("/repo");
    expect(byId("root-path").title).toBe("Opened from GESTALT_ROOT");
    expect(localStorage.getItem("gestalt.lastRoot")).toBe("/repo");
    expect(m.resetCommits).toHaveBeenCalledWith("/repo");
    expect(m.setTitle).toHaveBeenCalledWith("repo — Gestalt");
    expect(byId("welcome").hidden).toBe(true);
    expect(byId("workspace").hidden).toBe(false);
    expect(byId<HTMLButtonElement>("refresh-tree").disabled).toBe(false);
  });

  it("falls back to the last folder when the configured root can't be read", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    await boot({
      lastRoot: "/repo",
      handlers: {
        initial_root: () => {
          throw "bad args";
        },
      },
    });
    expect(error).toHaveBeenCalledWith("bad args");
    expect(invoked("open_root")).toEqual([{ path: "/repo" }]);
    expect(byId("root-path").title).toBe("");
  });

  it("shows the welcome panel when storage is unavailable", async () => {
    vi.spyOn(Storage.prototype, "getItem").mockImplementation(() => {
      throw new Error("denied");
    });
    await boot();
    expect(byId("welcome").hidden).toBe(false);
    expect(invoked("open_root")).toEqual([]);
  });

  it("still opens a folder when it can't be remembered", async () => {
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => {
      throw new Error("denied");
    });
    await boot({ handlers: { initial_root: () => ({ path: "/repo", source: "cli" }) } });
    expect(byId("root-path").textContent).toBe("/repo");
    expect(byId("workspace").hidden).toBe(false);
  });

  it("picks a folder from the toolbar, the welcome button or ⌘O / Ctrl+O", async () => {
    await boot();
    m.open.mockResolvedValueOnce(null);
    byId("welcome-open").click();
    await flush();
    expect(m.open).toHaveBeenCalledWith({
      directory: true,
      multiple: false,
      title: "Open infrastructure folder",
      defaultPath: undefined,
    });
    expect(invoked("open_root")).toEqual([]);

    m.open.mockResolvedValueOnce("/repo");
    byId("open-root").click();
    await flush();
    expect(invoked("open_root")).toEqual([{ path: "/repo" }]);

    m.open.mockResolvedValueOnce(["/a", "/b"]);
    expect(key("o", { metaKey: true }).defaultPrevented).toBe(true);
    await flush();
    expect(m.open).toHaveBeenLastCalledWith(expect.objectContaining({ defaultPath: "/repo" }));
    expect(invoked("open_root")).toHaveLength(1);

    m.open.mockResolvedValueOnce("/other");
    key("O", { ctrlKey: true });
    await flush();
    expect(byId("root-path").textContent).toBe("/other");

    expect(key("o").defaultPrevented).toBe(false);
    expect(m.open).toHaveBeenCalledTimes(4);
  });

  it("shows why a folder couldn't be opened", async () => {
    await boot({
      lastRoot: "/gone",
      handlers: {
        open_root: () => {
          throw "No such file or directory";
        },
      },
    });
    expect(byId("welcome").hidden).toBe(false);
    expect(byId("welcome-error").hidden).toBe(false);
    expect(byId("welcome-error").textContent).toBe("Could not open /gone: No such file or directory");
    expect(byId("tree").children).toHaveLength(0);
    expect(byId<HTMLButtonElement>("refresh-tree").disabled).toBe(false);
  });

  it("only applies the latest of overlapping tree builds", async () => {
    await bootRepo();
    const btn = byId<HTMLButtonElement>("refresh-tree");
    const first = deferred<DirEntry>();
    const second = deferred<DirEntry>();
    const pending = [first, second];
    handlers.open_root = () => pending.shift()!.promise;

    // The button is disabled while building, but ⌘R still gets through.
    btn.click();
    key("F5");
    expect(btn.disabled).toBe(true);
    second.resolve(dir("/repo", { name: "second", hasChildren: false }));
    await flush();
    expect(btn.disabled).toBe(false);
    first.resolve(dir("/repo", { name: "first", hasChildren: false }));
    await flush();
    expect(treeLabels()).toEqual(["second"]);

    // A stale failure is ignored too, and doesn't re-enable the button early.
    const third = deferred<DirEntry>();
    const fourth = deferred<DirEntry>();
    pending.push(third, fourth);
    key("F5");
    key("F5");
    third.reject("stale");
    await flush();
    expect(btn.disabled).toBe(true);
    expect(byId("welcome").hidden).toBe(true);
    fourth.resolve(dir("/repo", { name: "fourth", hasChildren: false }));
    await flush();
    expect(treeLabels()).toEqual(["fourth"]);
    expect(btn.disabled).toBe(false);
  });

  it("refreshes on ⌘R / Ctrl+R / F5 and the Refresh button", async () => {
    await bootRepo();
    byId("refresh-tree").click();
    expect(key("F5").defaultPrevented).toBe(true);
    key("r", { ctrlKey: true });
    key("R", { metaKey: true });
    expect(key("r").defaultPrevented).toBe(false);
    await flush();
    expect(invoked("open_root")).toHaveLength(5);
    expect(m.refreshCommits).toHaveBeenCalledTimes(4);
  });

  it("doesn't build a tree before a folder is open", async () => {
    await boot();
    key("F5");
    await flush();
    expect(invoked("open_root")).toEqual([]);
    expect(m.refreshCommits).toHaveBeenCalledTimes(1);
  });
});

describe("folder tree", () => {
  it("shows the root expanded with folder and file rows", async () => {
    await bootRepo();
    expect(treeLabels()).toEqual(["/repo", "envs", "empty", "broken", "README.md"]);
    const root = row("/repo");
    expect(root.querySelector(".caret")!.textContent).toBe("▾");
    expect(root.querySelector(".icon")!.innerHTML).toBe(html(icons.folderOpen));
    expect(root.style.paddingLeft).toBe("6px");

    const envs = row("envs");
    expect(envs.className).toBe("tree-row folder");
    expect(envs.querySelector(".caret")!.textContent).toBe("▸");
    expect(envs.querySelector(".icon")!.innerHTML).toBe(html(icons.folder));
    expect(envs.querySelector(".badge")).toBeNull();
    expect(envs.style.paddingLeft).toBe("20px");
    expect(row("empty").querySelector(".caret")!.textContent).toBe("");

    const readme = row("README.md");
    expect(readme.className).toBe("tree-row file file-text");
    expect(readme.title).toBe("/repo/README.md");
    expect(readme.querySelector(".icon")!.innerHTML).toBe(html(icons.fileText));
  });

  it("expands and collapses folders, loading children once", async () => {
    await bootRepo();
    row("envs").click();
    await flush();
    expect(treeLabels()).toEqual(["/repo", "envs", "prod", "common.tfvars", "empty", "broken", "README.md"]);
    expect(row("envs").classList.contains("selected")).toBe(true);
    expect(row("envs").querySelector(".caret")!.textContent).toBe("▾");

    const prod = row("prod");
    expect(prod.className).toBe("tree-row folder module");
    expect(prod.querySelector(".badge")!.textContent).toBe("tf");
    prod.click();
    await flush();
    expect(row("main.tf").className).toBe("tree-row file file-tofu");
    expect(row("prod.tfvars").className).toBe("tree-row file file-vars");
    expect(row("prod.tfvars").style.paddingLeft).toBe("48px");

    const listCalls = invoked("list_dir").length;
    row("envs").click();
    await flush();
    expect(treeLabels()).toEqual(["/repo", "envs", "empty", "broken", "README.md"]);
    expect(row("envs").querySelector(".icon")!.innerHTML).toBe(html(icons.folder));
    row("envs").click();
    await flush();
    expect(treeLabels()).toContain("prod");
    expect(invoked("list_dir")).toHaveLength(listCalls);
  });

  it("selects a childless folder or a file without expanding", async () => {
    await bootRepo();
    row("empty").click();
    await flush();
    expect(row("empty").classList.contains("selected")).toBe(true);
    expect(invoked("list_dir")).not.toContainEqual({ path: "/repo/empty" });
    row("README.md").click();
    expect(row("README.md").classList.contains("selected")).toBe(true);
    expect(document.querySelectorAll(".tree-row.selected")).toHaveLength(1);
  });

  it("logs a folder that can't be listed and leaves it closed", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    await bootRepo();
    row("broken").click();
    await flush();
    expect(error).toHaveBeenCalledWith(new Error("permission denied"));
    expect(row("broken").querySelector(".caret")!.textContent).toBe("▸");
  });

  it("keeps expansion and selection across refreshes, dropping vanished folders", async () => {
    await bootRepo();
    row("envs").click();
    await flush();
    row("prod").click();
    await flush();
    row("main.tf").click();

    // envs/prod becomes unreadable: it's collapsed (and forgotten) on refresh.
    const realList = handlers.list_dir;
    handlers.list_dir = (a) => {
      if (a!.path === "/repo/envs/prod") throw new Error("gone");
      return realList(a);
    };
    byId("refresh-tree").click();
    await flush();
    expect(treeLabels()).toEqual(["/repo", "envs", "prod", "common.tfvars", "empty", "broken", "README.md"]);
    expect(row("prod").querySelector(".caret")!.textContent).toBe("▸");

    handlers.list_dir = realList;
    byId("refresh-tree").click();
    await flush();
    expect(treeLabels()).not.toContain("main.tf");

    // Selection survives a rebuild.
    row("prod").click();
    await flush();
    row("main.tf").click();
    byId("refresh-tree").click();
    await flush();
    expect(row("main.tf").classList.contains("selected")).toBe(true);
    row("envs").click();
    byId("refresh-tree").click();
    await flush();
    expect(row("envs").classList.contains("selected")).toBe(true);
  });

  it("builds a root that isn't a folder as a plain row", async () => {
    await bootRepo({ handlers: { open_root: () => file("/repo") } });
    expect(treeLabels()).toEqual(["repo"]);
    expect(row("repo").classList.contains("folder")).toBe(true);
  });

  it("marks folders that have cloud settings of their own", async () => {
    await bootRepo({
      handlers: { cloud_folders: () => ({ "/repo/envs": { "aws.profile": "dev", "gcp.project": "p-1" } }) },
    });
    const logos = [...row("envs").querySelectorAll<HTMLElement>(".cloud-logo")];
    expect(logos.map((b) => b.className)).toEqual(["cloud-logo aws", "cloud-logo gcp"]);
    expect(logos[0].querySelector("svg")).not.toBeNull();
    expect(logos[0].title).toBe("AWS profile: dev\nApplies here and to all subfolders.");
    expect(row("empty").querySelector(".badge, .cloud-logo")).toBeNull();
  });

  it("builds the tree without badges when cloud settings can't be read", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    await bootRepo({
      handlers: {
        cloud_folders: () => {
          throw "unreadable";
        },
      },
    });
    expect(error).toHaveBeenCalledWith("unreadable");
    expect(treeLabels()).toContain("envs");
    expect(document.querySelector(".tree .cloud-logo")).toBeNull();
  });
});

describe("context menus", () => {
  it("offers tofu commands for module folders and settings for any folder", async () => {
    await bootRepo();
    row("envs").click();
    await flush();
    const ev = rightClick(row("prod"));
    await flush();
    expect(ev.defaultPrevented).toBe(true);
    expect(row("prod").classList.contains("selected")).toBe(true);
    expect(menuTexts()).toEqual(["Import…", "Plan", "Apply…", "--Separator", "Cloud Settings…", "Clean .terraform Caches…"]);
    expect(lastMenu().popup).toHaveBeenCalled();

    rightClick(row("envs"));
    await flush();
    expect(menuTexts()).toEqual(["Cloud Settings…", "Clean .terraform Caches…"]);
  });

  it("offers a file's folder menu when right-clicking a file", async () => {
    await bootRepo();
    row("envs").click();
    await flush();
    row("prod").click();
    await flush();
    rightClick(row("main.tf"));
    await flush();
    expect(row("main.tf").classList.contains("selected")).toBe(true);
    expect(menuTexts()).toContain("Plan");
    menuAction("Plan");
    await flush();
    expect(invoked("run_plan")).toEqual([{ dir: "/repo/envs/prod" }]);

    rightClick(row("README.md"));
    await flush();
    expect(menuTexts()).toEqual(["Cloud Settings…", "Clean .terraform Caches…"]);
    menuAction("Cloud Settings…");
    expect(m.openCloudSettings).toHaveBeenCalledWith("/repo", "repo", expect.any(Function));
    rightClick(row("envs"));
    await flush();
    menuAction("Cloud Settings…");
    const [dir, shown, rel] = m.openCloudSettings.mock.lastCall!;
    expect([dir, shown, rel("/repo/envs/prod")]).toEqual(["/repo/envs", "envs", "envs/prod"]);
  });

  it("runs Plan and Apply… from the menu", async () => {
    await bootRepo();
    row("envs").click();
    await flush();
    rightClick(row("prod"));
    await flush();
    menuAction("Apply…");
    await flush();
    expect(invoked("run_plan")).toEqual([{ dir: "/repo/envs/prod" }]);
    expect(tabLabels()).toEqual(["apply · envs/prod"]);
  });

  it("imports with the dialog's request, or not at all when cancelled", async () => {
    await bootRepo({ handlers: { run_import: () => 9 } });
    row("envs").click();
    await flush();
    rightClick(row("prod"));
    await flush();
    menuAction("Import…");
    await flush();
    expect(m.openImportDialog).toHaveBeenCalledWith("/repo/envs/prod", "envs/prod");
    expect(invoked("run_import")).toEqual([]);

    const request = { address: "a.b", id: "x", vars: [] };
    m.openImportDialog.mockResolvedValueOnce(request);
    menuAction("Import…");
    await flush();
    expect(invoked("run_import")).toEqual([{ dir: "/repo/envs/prod", request }]);
    expect(tabLabels()).toEqual(["import · envs/prod"]);
  });

  it("suppresses the webview menu except in text fields", async () => {
    await boot();
    const ctx = (el: HTMLElement) => {
      document.body.append(el);
      return rightClick(el).defaultPrevented;
    };
    expect(ctx(document.createElement("div"))).toBe(true);
    expect(ctx(document.createElement("input"))).toBe(false);
    expect(ctx(document.createElement("textarea"))).toBe(false);
  });
});

describe("workspace changes", () => {
  it("ignores changes with no folder open", async () => {
    await boot();
    emit("workspace://changed", { structure: true, gitRefs: true, worktree: true });
    await flush();
    expect(invoked("open_root")).toEqual([]);
    expect(m.refreshCommits).not.toHaveBeenCalled();
    expect(m.refreshUncommitted).not.toHaveBeenCalled();
  });

  it("refreshes only the affected views", async () => {
    await bootRepo();
    const change = (c: Record<string, boolean>) =>
      emit("workspace://changed", { worktree: false, structure: false, folders: false, gitIndex: false, gitRefs: false, ...c });

    change({ structure: true });
    await flush();
    expect(invoked("open_root")).toHaveLength(2);
    expect(m.refreshCommits).not.toHaveBeenCalled();
    expect(m.refreshUncommitted).not.toHaveBeenCalled();

    change({ gitRefs: true, worktree: true });
    change({ folders: true });
    expect(m.refreshCommits).toHaveBeenCalledTimes(2);
    expect(m.refreshUncommitted).not.toHaveBeenCalled();

    change({ worktree: true });
    change({ gitIndex: true });
    expect(m.refreshUncommitted).toHaveBeenCalledTimes(2);

    change({});
    await flush();
    expect(invoked("open_root")).toHaveLength(2);
    expect(m.refreshCommits).toHaveBeenCalledTimes(2);
    expect(m.refreshUncommitted).toHaveBeenCalledTimes(2);
  });
});

describe("runs", () => {
  it("shows a plan's output and outcome in its own tab", async () => {
    await bootRepo();
    plan("/repo/envs/prod");
    await flush();
    expect(invoked("run_plan")).toEqual([{ dir: "/repo/envs/prod" }]);
    expect(tabLabels()).toEqual(["plan · envs/prod"]);
    expect(tabs()[0].className).toBe("run-tab running selected");
    expect(tabs()[0].title).toBe("/repo/envs/prod");
    expect(closeButton(0).hidden).toBe(true);
    expect(closeButton(0).getAttribute("aria-label")).toBe("Close plan · envs/prod");
    // Label first, close button second, off macOS.
    expect([...tabs()[0].children].map((c) => c.className)).toEqual(["run-tab-label", "run-tab-close"]);
    expect(byId("run-header").hidden).toBe(false);
    expect(byId("run-title").textContent).toBe("tofu plan — /repo/envs/prod");
    expect(byId("run-status").className).toBe("status running");
    expect(status()).toBe("running…");
    expect(byId("run-cancel").hidden).toBe(false);
    expect(byId("run-apply").hidden).toBe(true);

    out(1, "\x1b[32mgreen");
    out(1, "still green");
    out(1, "warning", "stderr");
    out(1, "cloud: AWS profile prd", "gestalt");
    out(1, PLAN_LINE);
    expect(output().innerHTML).toBe(
      '<span class="ansi-fg-2">green</span>\n<span class="ansi-fg-2">still green</span>\n' +
        '<span class="stderr">warning\n</span><span class="gestalt">cloud: AWS profile prd\n</span>' +
        `<span class="ansi-fg-2">${PLAN_LINE}</span>\n`,
    );

    exitEv(1, { hasChanges: true });
    expect(status()).toBe(PLAN_LINE);
    expect(byId("run-status").className).toBe("status succeeded");
    expect(tabs()[0].className).toBe("run-tab succeeded plan-ready selected");
    expect(byId("run-apply").hidden).toBe(false);
    expect(byId("run-cancel").hidden).toBe(true);
    expect(closeButton(0).hidden).toBe(false);
    // Reselecting re-renders from scratch with fresh colour state.
    tabs()[0].click();
    expect(output().textContent).toBe(`green\nstill green\nwarning\ncloud: AWS profile prd\n${PLAN_LINE}\n`);
  });

  it("puts the close button first on macOS", async () => {
    await bootRepo({ userAgent: "Macintosh" });
    plan("/repo/envs/prod");
    await flush();
    expect([...tabs()[0].children].map((c) => c.className)).toEqual(["run-tab-close", "run-tab-label"]);
  });

  it("labels runs relative to the open folder", async () => {
    await boot();
    plan("/elsewhere/app");
    await flush();
    expect(tabLabels()).toEqual(["plan · /elsewhere/app"]);

    vi.resetModules();
    loadIndexHtml();
    let id = 10;
    await bootRepo({ handlers: { run_plan: () => id++ } });
    plan("/repo");
    plan("/other/app");
    await flush();
    expect(tabLabels()).toEqual(["plan · repo", "plan · /other/app"]);

    vi.resetModules();
    loadIndexHtml();
    await boot({ lastRoot: "/", handlers: { run_plan: () => 3 } });
    plan("/");
    await flush();
    expect(tabLabels()).toEqual(["plan · /"]);

    vi.resetModules();
    loadIndexHtml();
    await boot({ lastRoot: "C:\\infra", handlers: { run_plan: () => 4 } });
    plan("C:\\infra\\net");
    await flush();
    expect(tabLabels()).toEqual(["plan · net"]);
  });

  it("keeps output and exit events that arrive before the run id", async () => {
    await bootRepo();
    const d = deferred<number>();
    handlers.run_plan = () => d.promise;
    plan("/repo/envs/prod");
    await flush();
    out(5, "early 1");
    out(5, "early 2");
    exitEv(5, { hasChanges: false });
    d.resolve(5);
    await flush();
    expect(output().textContent).toBe("early 1\nearly 2\n");
    expect(status()).toBe("no changes");

    // An exit with no output first.
    const d2 = deferred<number>();
    handlers.run_plan = () => d2.promise;
    plan("/repo/envs/prod");
    await flush();
    exitEv(6, { success: false, code: 1, error: "init failed" });
    d2.resolve(6);
    await flush();
    expect(output().textContent).toBe("init failed\n");
    expect(status()).toBe("failed (exit 1)");
  });

  it("shows a run that couldn't be started", async () => {
    await bootRepo({
      handlers: {
        run_plan: () => {
          throw "backend unavailable";
        },
      },
    });
    plan("/repo/envs/prod");
    await flush();
    expect(tabLabels()).toEqual(["plan · envs/prod"]);
    expect(output().innerHTML).toBe('<span class="stderr">backend unavailable\n</span>');
    expect(status()).toBe("failed");
  });

  it("follows output only when scrolled to the bottom, and only for the shown run", async () => {
    await bootRepo();
    handlers.run_plan = () => 1;
    plan("/repo/a");
    await flush();
    handlers.run_plan = () => 2;
    plan("/repo/b");
    await flush();
    expect(selectedTab()).toBe(1);

    let top = 0;
    const el = output();
    Object.defineProperty(el, "scrollHeight", { configurable: true, get: () => 1000 });
    Object.defineProperty(el, "clientHeight", { configurable: true, get: () => 100 });
    Object.defineProperty(el, "scrollTop", { configurable: true, get: () => top, set: (v) => (top = v) });

    top = 880; // within 40px of the bottom
    out(2, "b1");
    expect(top).toBe(1000);
    top = 500; // scrolled up to read
    out(2, "b2");
    expect(top).toBe(500);

    out(1, "a1");
    expect(output().textContent).toBe("b1\nb2\n");
    tabs()[0].click();
    expect(output().textContent).toBe("a1\n");
    expect(top).toBe(1000);
  });

  it("closes finished tabs, picking the last remaining one", async () => {
    await bootRepo();
    for (const id of [1, 2, 3]) {
      handlers.run_plan = () => id;
      plan(`/repo/r${id}`);
      await flush();
    }
    // Running tabs can't be closed by middle-click.
    tabs()[1].dispatchEvent(new MouseEvent("auxclick", { button: 1 }));
    expect(tabs()).toHaveLength(3);

    exitEv(1);
    exitEv(2);
    exitEv(3);
    tabs()[0].click();
    expect(selectedTab()).toBe(0);
    // Other buttons don't close.
    tabs()[1].dispatchEvent(new MouseEvent("auxclick", { button: 2 }));
    expect(tabs()).toHaveLength(3);
    // Closing an unselected tab keeps the selection.
    tabs()[1].dispatchEvent(new MouseEvent("auxclick", { button: 1 }));
    expect(tabLabels()).toEqual(["plan · r1", "plan · r3"]);
    expect(selectedTab()).toBe(0);
    // Closing the selected tab selects the last one (and doesn't reselect the closed one).
    closeButton(0).click();
    expect(tabLabels()).toEqual(["plan · r3"]);
    expect(selectedTab()).toBe(0);
    closeButton(0).click();
    expect(tabs()).toHaveLength(0);
    expect(byId("run-header").hidden).toBe(true);
    expect(output().textContent).toBe("");
    expect(invoked("discard_plan")).toEqual([]);
  });

  it("discards a ready plan when its tab is closed", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    await bootRepo({ handlers: { discard_plan: () => undefined } });
    plan("/repo/a");
    await flush();
    exitEv(1, { hasChanges: true });
    closeButton(0).click();
    expect(invoked("discard_plan")).toEqual([{ planRunId: 1 }]);

    handlers.discard_plan = () => {
      throw "already gone";
    };
    plan("/repo/a");
    await flush();
    exitEv(1, { hasChanges: true });
    closeButton(0).click();
    await flush();
    expect(error).toHaveBeenCalledWith("already gone");
  });

  it("cancels the selected run", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    await bootRepo({ handlers: { cancel_tofu: () => undefined } });
    byId("run-cancel").click();
    expect(invoked("cancel_tofu")).toEqual([]);

    handlers.run_plan = () => 4;
    plan("/repo/a");
    await flush();
    byId("run-cancel").click();
    expect(invoked("cancel_tofu")).toEqual([{ runId: 4 }]);
    exitEv(4, { cancelled: true, success: false, code: 130 });
    expect(status()).toBe("cancelled");

    handlers.cancel_tofu = () => {
      throw "not running";
    };
    byId("run-cancel").click();
    await flush();
    expect(error).toHaveBeenCalledWith("not running");
  });

  it("updates an unselected run's tab without disturbing the shown run", async () => {
    await bootRepo();
    handlers.run_plan = () => 1;
    plan("/repo/a");
    await flush();
    handlers.run_plan = () => 2;
    plan("/repo/b");
    await flush();
    exitEv(1, { success: false, code: 2 });
    expect(tabs()[0].className).toBe("run-tab failed");
    expect(byId("run-title").textContent).toBe("tofu plan — /repo/b");
    expect(status()).toBe("running…");
  });
});

describe("applying a plan", () => {
  async function readyPlan(extra: Record<string, Handler> = {}) {
    await bootRepo({ handlers: { apply_plan: () => undefined, ...extra } });
    plan("/repo/app");
    await flush();
    out(1, PLAN_LINE);
    exitEv(1, { hasChanges: true });
  }

  it("asks, then applies the saved plan in the same tab", async () => {
    await readyPlan();
    m.ask.mockResolvedValueOnce(true);
    byId("run-apply").click();
    await flush();
    expect(m.ask).toHaveBeenCalledWith(
      `Apply this plan to\n/repo/app?\n\n${PLAN_LINE}\n\nThis will change real infrastructure.`,
      { title: "Apply plan", kind: "warning", okLabel: "Apply", cancelLabel: "Cancel" },
    );
    expect(invoked("apply_plan")).toEqual([{ planRunId: 1 }]);
    expect(tabLabels()).toEqual(["apply · app"]);
    expect(byId("run-title").textContent).toBe("tofu apply — /repo/app");
    expect(status()).toBe("applying…");
    expect(byId("run-apply").hidden).toBe(true);
    expect(tabs()[0].classList.contains("plan-ready")).toBe(false);

    out(1, "Apply complete!");
    exitEv(1);
    expect(status()).toBe(`applied — ${PLAN_LINE}`);
    expect(byId("run-apply").hidden).toBe(true);
  });

  it("does nothing when declined", async () => {
    await readyPlan();
    m.ask.mockResolvedValueOnce(false);
    byId("run-apply").click();
    await flush();
    expect(invoked("apply_plan")).toEqual([]);
    expect(status()).toBe(PLAN_LINE);
  });

  it("uses a generic summary when tofu printed none", async () => {
    await bootRepo({ handlers: { apply_plan: () => undefined } });
    plan("/repo/app");
    await flush();
    exitEv(1, { hasChanges: true });
    m.ask.mockResolvedValueOnce(false);
    byId("run-apply").click();
    await flush();
    expect(m.ask.mock.calls[0][0]).toContain("\n\nThe plan has changes.\n\n");
  });

  it("keeps the plan applicable when the apply can't start", async () => {
    await readyPlan({
      apply_plan: () => {
        throw "folder busy";
      },
    });
    m.ask.mockResolvedValueOnce(true);
    byId("run-apply").click();
    await flush();
    expect(byId("run-apply").hidden).toBe(false);
    expect(output().textContent).toContain("folder busy\n");
    expect(status()).toBe("failed");

    // Same via an exit event reporting the apply never started.
    m.ask.mockResolvedValueOnce(true);
    handlers.apply_plan = () => undefined;
    byId("run-apply").click();
    await flush();
    exitEv(1, { success: false, code: null, started: false, error: "env error" });
    expect(byId("run-apply").hidden).toBe(false);

    // An apply that ran and failed leaves nothing to apply.
    m.ask.mockResolvedValueOnce(true);
    byId("run-apply").click();
    await flush();
    exitEv(1, { success: false, code: 1 });
    expect(byId("run-apply").hidden).toBe(true);
    expect(status()).toBe("failed (exit 1)");
  });

  it("doesn't apply a plan whose tab was closed while asking", async () => {
    await readyPlan({ discard_plan: () => undefined });
    const answer = deferred<boolean>();
    m.ask.mockReturnValueOnce(answer.promise);
    byId("run-apply").click();
    await flush();
    closeButton(0).click();
    answer.resolve(true);
    await flush();
    expect(invoked("discard_plan")).toEqual([{ planRunId: 1 }]);
    expect(invoked("apply_plan")).toEqual([]);
  });

  it("applies without redrawing the header when another tab is shown", async () => {
    await readyPlan();
    handlers.run_plan = () => 2;
    const answer = deferred<boolean>();
    m.ask.mockReturnValueOnce(answer.promise);
    byId("run-apply").click();
    await flush();
    plan("/repo/other");
    await flush();
    answer.resolve(true);
    await flush();
    expect(invoked("apply_plan")).toEqual([{ planRunId: 1 }]);
    expect(byId("run-title").textContent).toBe("tofu plan — /repo/other");
    expect(tabLabels()).toEqual(["apply · app", "plan · other"]);
  });

  it("ignores a stale Apply click", async () => {
    await readyPlan();
    m.ask.mockResolvedValueOnce(true);
    byId("run-apply").click();
    await flush();
    // The button is hidden now; a click that still gets through does nothing.
    byId("run-apply").click();
    await flush();
    expect(m.ask).toHaveBeenCalledTimes(1);
  });

  it("ignores Apply with no run shown", async () => {
    await bootRepo();
    byId("run-apply").click();
    await flush();
    expect(m.ask).not.toHaveBeenCalled();
  });

  it("Apply… asks once the plan is ready, selecting its tab", async () => {
    await bootRepo({ handlers: { apply_plan: () => undefined } });
    applyPlan("/repo/app");
    await flush();
    expect(tabLabels()).toEqual(["apply · app"]);
    handlers.run_plan = () => 2;
    plan("/repo/other");
    await flush();
    expect(selectedTab()).toBe(1);

    m.ask.mockResolvedValueOnce(false);
    out(1, PLAN_LINE);
    exitEv(1, { hasChanges: true });
    await flush();
    expect(selectedTab()).toBe(0);
    expect(m.ask).toHaveBeenCalledTimes(1);
    expect(status()).toBe(`${PLAN_LINE} (not applied)`);

    // Only once: the Apply button covers retries.
    m.ask.mockResolvedValueOnce(true);
    byId("run-apply").click();
    await flush();
    expect(invoked("apply_plan")).toEqual([{ planRunId: 1 }]);
  });

  it("Apply… with no changes doesn't ask", async () => {
    await bootRepo();
    applyPlan("/repo/app");
    await flush();
    exitEv(1, { hasChanges: false });
    await flush();
    expect(m.ask).not.toHaveBeenCalled();
    expect(status()).toBe("no changes");
  });
});

describe("cleaning caches", () => {
  const found = { paths: ["/repo/a/.terraform", "/repo/b/.terraform.lock.hcl"], dirs: 1, files: 1, bytes: 2048, errors: [] };

  async function cleanFrom(handlersExtra: Record<string, Handler>) {
    await bootRepo({ handlers: handlersExtra });
    emit("menu://clean-all");
    await flush();
  }

  it("does nothing from the app menu with no folder open", async () => {
    await boot();
    emit("menu://clean-all");
    await flush();
    expect(invoked("clean_scan")).toEqual([]);
  });

  it("reports a failed scan", async () => {
    await cleanFrom({
      clean_scan: () => {
        throw "walk failed";
      },
    });
    expect(invoked("clean_scan")).toEqual([{ dir: "/repo" }]);
    expect(m.message).toHaveBeenCalledWith("walk failed", { title: "Clean caches", kind: "error" });
    expect(m.ask).not.toHaveBeenCalled();
  });

  it("says when there's nothing to clean", async () => {
    await cleanFrom({ clean_scan: () => ({ ...found, paths: [], dirs: 0, files: 0, bytes: 0 }) });
    expect(m.message).toHaveBeenCalledWith("No .terraform folders or lock files under\n/repo", { title: "Clean caches" });
    expect(m.ask).not.toHaveBeenCalled();
  });

  it("asks first, and does nothing when declined", async () => {
    m.ask.mockResolvedValueOnce(false);
    await cleanFrom({ clean_scan: () => found });
    expect(m.ask).toHaveBeenCalledWith(
      "Remove 1 .terraform folder and 1 .terraform.lock.hcl file (2.0 KB) under\n/repo?\n\n" +
        "The next plan in each module runs tofu init to download them again.",
      { title: "Clean caches", kind: "warning", okLabel: "Remove", cancelLabel: "Cancel" },
    );
    expect(invoked("clean_caches")).toEqual([]);
    expect(tabs()).toHaveLength(0);
  });

  it("lists what was removed in a clean tab", async () => {
    const done = deferred<unknown>();
    m.ask.mockResolvedValueOnce(true);
    await cleanFrom({ clean_scan: () => found, clean_caches: () => done.promise });
    expect(tabLabels()).toEqual(["clean · repo"]);
    expect(byId("run-title").textContent).toBe("clean caches — /repo");
    expect(status()).toBe("cleaning…");
    expect(byId("run-cancel").hidden).toBe(true);

    done.resolve({ ...found, errors: [] });
    await flush();
    expect(output().innerHTML).toBe(
      "removed a/.terraform\nremoved b/.terraform.lock.hcl\n" + '<span class="gestalt">Freed 2.0 KB.\n</span>',
    );
    expect(status()).toBe("freed 2.0 KB");
    expect(tabs()[0].className).toBe("run-tab succeeded selected");
  });

  it("shows removal errors and empty results", async () => {
    m.ask.mockResolvedValue(true);
    await cleanFrom({
      clean_scan: () => found,
      clean_caches: () => ({ ...found, paths: [], bytes: 0, errors: ["/repo/a/.terraform: busy"] }),
    });
    expect(output().innerHTML).toBe(
      '<span class="stderr">/repo/a/.terraform: busy\n</span><span class="gestalt">Freed 0 bytes.\n</span>',
    );
    expect(status()).toBe("nothing removed, with errors");

    vi.spyOn(Date, "now").mockReturnValue(123);
    handlers.clean_caches = () => ({ ...found, errors: ["x: busy"] });
    emit("menu://clean-all");
    await flush();
    expect(status()).toBe("freed 2.0 KB, with errors");

    vi.spyOn(Date, "now").mockReturnValue(456);
    handlers.clean_caches = () => ({ ...found, paths: [], bytes: 0 });
    emit("menu://clean-all");
    await flush();
    expect(status()).toBe("nothing removed");
  });

  it("shows a failed clean", async () => {
    m.ask.mockResolvedValueOnce(true);
    await cleanFrom({
      clean_scan: () => found,
      clean_caches: () => {
        throw "disk error";
      },
    });
    expect(output().innerHTML).toBe('<span class="stderr">disk error\n</span>');
    expect(status()).toBe("failed");
  });

  it("finishes quietly in the background or after its tab is gone", async () => {
    const done = deferred<unknown>();
    m.ask.mockResolvedValue(true);
    await cleanFrom({ clean_scan: () => found, clean_caches: () => done.promise });
    plan("/repo/other");
    await flush();
    done.resolve(found);
    await flush();
    expect(tabs()[0].className).toBe("run-tab succeeded");
    expect(byId("run-title").textContent).toBe("tofu plan — /repo/other");

    // Closed while cleaning (its close button is hidden, but be safe): no tab comes back.
    vi.spyOn(Date, "now").mockReturnValue(789);
    const again = deferred<unknown>();
    handlers.clean_caches = () => again.promise;
    emit("menu://clean-all");
    await flush();
    expect(tabLabels()).toEqual(["clean · repo", "plan · other", "clean · repo"]);
    closeButton(2).click();
    again.resolve(found);
    await flush();
    expect(tabLabels()).toEqual(["clean · repo", "plan · other"]);
  });

  it("cleans a folder from its context menu", async () => {
    await bootRepo({ handlers: { clean_scan: () => ({ ...found, paths: [] }) } });
    rightClick(row("envs"));
    await flush();
    menuAction("Clean .terraform Caches…");
    await flush();
    expect(invoked("clean_scan")).toEqual([{ dir: "/repo/envs" }]);
  });
});

describe("cloud settings", () => {
  const prd = [
    { key: "aws.profile", value: "prd", source: "/repo" },
    { key: "aws.region", value: "eu-west-1", source: "/repo/envs" },
  ];
  const statusButton = () => byId<HTMLButtonElement>("cloud-status");

  it("shows what applies to the open folder and edits it from the toolbar", async () => {
    await boot();
    expect(statusButton().hidden).toBe(true);
    statusButton().click();
    expect(m.openCloudSettings).not.toHaveBeenCalled();

    handlers.cloud_effective = () => prd;
    m.open.mockResolvedValueOnce("/repo");
    byId("open-root").click();
    await flush();
    expect(invoked("cloud_effective")).toEqual([{ dir: "/repo" }]);
    expect(statusButton().hidden).toBe(false);
    expect(byId("cloud-status-text").textContent).toBe("prd · eu-west-1");
    const logo = byId("cloud-status-text").querySelector<HTMLElement>(".cloud-logo.aws")!;
    expect(logo.title).toBe("AWS");
    expect(statusButton().querySelector<HTMLElement>(".icon")!.hidden).toBe(true);
    expect(statusButton().classList.contains("unset")).toBe(false);
    expect(statusButton().title).toMatch(/^tofu runs here use AWS prd · eu-west-1\./);

    statusButton().click();
    expect(m.openCloudSettings).toHaveBeenCalledWith("/repo", "repo", expect.any(Function));
  });

  it("says when no account is chosen, or settings can't be read", async () => {
    await bootRepo();
    expect(byId("cloud-status-text").textContent).toBe("No cloud account");
    expect(statusButton().querySelector<HTMLElement>(".icon")!.hidden).toBe(false);
    expect(statusButton().classList.contains("unset")).toBe(true);
    expect(statusButton().title).toMatch(/Click to choose one/);

    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    handlers.cloud_effective = () => {
      throw "broken";
    };
    emit("cloud://changed");
    await flush();
    expect(error).toHaveBeenCalledWith("broken");
    expect(byId("cloud-status-text").textContent).toBe("No cloud account");
  });

  it("refreshes the tree and toolbar when any window changes settings", async () => {
    await boot();
    emit("cloud://changed");
    await flush();
    expect(invoked("open_root")).toEqual([]);

    m.open.mockResolvedValueOnce("/repo");
    byId("open-root").click();
    await flush();
    handlers.cloud_effective = () => prd;
    handlers.cloud_folders = () => ({ "/repo/envs": { "aws.region": "eu-west-1" } });
    emit("cloud://changed");
    await flush();
    expect(invoked("open_root")).toHaveLength(2);
    expect(byId("cloud-status-text").textContent).toBe("prd · eu-west-1");
    expect(row("envs").querySelector(".cloud-logo")!.className).toBe("cloud-logo aws");
  });

  it("ignores a slow answer for a folder that's no longer open", async () => {
    const slow = deferred<unknown>();
    await bootRepo({ handlers: { cloud_effective: () => slow.promise } });
    handlers.cloud_effective = () => [];
    m.open.mockResolvedValueOnce("/other");
    byId("open-root").click();
    await flush();
    slow.resolve(prd);
    await flush();
    expect(byId("cloud-status-text").textContent).toBe("No cloud account");
  });

  it("opens from the Workspace menu for the open folder only", async () => {
    await boot();
    emit("menu://cloud-settings");
    expect(m.openCloudSettings).not.toHaveBeenCalled();
    m.open.mockResolvedValueOnce("/repo");
    byId("open-root").click();
    await flush();
    emit("menu://cloud-settings");
    expect(m.openCloudSettings).toHaveBeenCalledWith("/repo", "repo", expect.any(Function));
  });
});

describe("windows", () => {
  it("opens no folder in a later window that wasn't given one", async () => {
    m.windowLabel = "workspace-2";
    await boot({ lastRoot: "/repo" });
    expect(invoked("open_root")).toEqual([]);
    expect(byId("welcome").hidden).toBe(false);
  });

  it("opens the folder a later window was given", async () => {
    m.windowLabel = "workspace-2";
    await boot({ handlers: { initial_root: () => ({ path: "/repo", source: "command line" }) } });
    expect(invoked("open_root")).toEqual([{ path: "/repo" }]);
  });

  it("logs a title that can't be set", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    m.setTitle.mockRejectedValue("no permission");
    await bootRepo();
    expect(error).toHaveBeenCalledWith("no permission");
  });

  it("asks before closing a window with tofu running, and stays open if declined", async () => {
    await bootRepo();
    const close = async () => {
      const ev = { preventDefault: vi.fn() };
      await closeRequested!(ev);
      return ev.preventDefault;
    };
    // Nothing running: closes without asking.
    expect(await close()).not.toHaveBeenCalled();
    expect(m.ask).not.toHaveBeenCalled();

    handlers.run_plan = () => 1;
    plan("/repo/envs/prod");
    await flush();
    m.ask.mockResolvedValueOnce(false);
    expect(await close()).toHaveBeenCalled();
    expect(m.ask).toHaveBeenLastCalledWith(
      "1 tofu run is still running in this window. Cancel and close?",
      expect.objectContaining({ okLabel: "Cancel runs and close", cancelLabel: "Keep open" }),
    );

    handlers.run_plan = () => 2;
    plan("/repo/envs");
    await flush();
    m.ask.mockResolvedValueOnce(true);
    expect(await close()).not.toHaveBeenCalled();
    expect(m.ask.mock.lastCall![0]).toBe("2 tofu runs are still running in this window. Cancel and close?");
  });

  it("logs a close handler that can't be installed", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    m.onCloseRequested.mockRejectedValue("no permission");
    await bootRepo();
    expect(error).toHaveBeenCalledWith("no permission");
  });
});
