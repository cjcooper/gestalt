import { invoke } from "@tauri-apps/api/core";
import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { ask, message, open } from "@tauri-apps/plugin-dialog";
import { newAnsiState, renderAnsi } from "./ansi";
import { cloudBadges, cloudParts, cloudSummary, openCloudSettings, providerLogo, type CloudSetting } from "./cloudSettings";
import { fileIcon, fileKind, icons } from "./icons";
import { openSettings } from "./settings";
import { PLAN_EVENT, closeEditorWindows, closeFrontDialog, type PlanRequest } from "./windows";
import { openImportDialog, type ImportRequest } from "./importDialog";
import { initSystemColors } from "./systemColors";
import { initSidebar, refreshCommits, refreshUncommitted, resetCommits } from "./sidebar";
import {
  describeClean,
  formatBytes,
  planSummary,
  statusText,
  type CleanSummary,
  type DirEntry,
  type ExitEvent,
  type OutputEvent,
  type Run,
  type RunKind,
} from "./runs";

// Each window has its own folder and runs. Listen only for events sent to
// this window (or to all windows), not those meant for another one.
const appWindow = getCurrentWebviewWindow();
const listen: typeof appWindow.listen = (event, handler) => appWindow.listen(event, handler);
/** The window opened at launch; later ones come from `gestalt <folder>` or New Window. */
const IS_LAUNCH_WINDOW = appWindow.label === "main";

// ---- Run state ---------------------------------------------------------------

const runs = new Map<number, Run>();
let selectedRunId: number | null = null;
// Output/exit events can arrive before the invoke resolves with the run id.
const early = new Map<number, { lines: Run["lines"]; exit?: ExitEvent }>();

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const LAST_ROOT_KEY = "gestalt.lastRoot";
const IS_MAC = navigator.userAgent.includes("Mac");
document.documentElement.dataset.platform = IS_MAC ? "mac" : "other";

function relName(dir: string): string {
  const root = currentRoot;
  if (root && dir.startsWith(root)) {
    const rel = dir.slice(root.length).replace(/^[\\/]/, "");
    return rel || baseName(root);
  }
  return dir;
}

function baseName(p: string): string {
  return p.split(/[\\/]/).filter(Boolean).pop() ?? p;
}

function registerRun(run: Run) {
  const pending = early.get(run.id);
  early.delete(run.id);
  if (pending) run.lines.push(...pending.lines);
  runs.set(run.id, run);
  selectRun(run.id);
  if (pending?.exit) handleExit(run, pending.exit);
}

/**
 * Start a backend run and give it a tab. The backend reports everything,
 * including failures to start, as output/exit events, so it all shows in the
 * run's pane. Only a failed invoke itself (shouldn't happen) is handled here.
 */
async function startRun(dir: string, kind: RunKind, command: string, args: Record<string, unknown>, extra: Partial<Run> = {}) {
  let id: number;
  try {
    id = await invoke<number>(command, args);
  } catch (e) {
    id = -Date.now(); // local-only id for a run that never reached the backend
    early.set(id, {
      lines: [],
      exit: { runId: id, code: null, success: false, cancelled: false, hasChanges: null, error: String(e), started: false },
    });
  }
  registerRun({ id, dir, kind, status: "running", lines: [], planState: "none", ansi: new Map(), ...extra });
}

/**
 * Plan in `dir`. With `applyWhenReady`, the run is an apply: init,
 * plan, confirm, then apply that saved plan, all in this one run's pane.
 */
const startPlan = (dir: string, applyWhenReady = false) =>
  startRun(dir, applyWhenReady ? "apply" : "plan", "run_plan", { dir }, { applyWhenReady });

async function startImport(dir: string) {
  const request: ImportRequest | null = await openImportDialog(dir, relName(dir));
  if (request) await startRun(dir, "import", "run_import", { dir, request });
}

/**
 * Remove every .terraform folder and .terraform.lock.hcl at or below `dir`
 * (the infra repo's cleanup-terraform-caches), after showing what will go.
 * The result is listed in its own run tab.
 */
async function cleanCaches(dir: string) {
  let found: CleanSummary;
  try {
    found = await invoke<CleanSummary>("clean_scan", { dir });
  } catch (e) {
    await message(String(e), { title: "Clean caches", kind: "error" });
    return;
  }
  if (!found.paths.length) {
    await message(`No .terraform folders or lock files under\n${dir}`, { title: "Clean caches" });
    return;
  }
  const ok = await ask(
    `Remove ${describeClean(found)} under\n${dir}?\n\nThe next plan in each module runs tofu init to download them again.`,
    { title: "Clean caches", kind: "warning", okLabel: "Remove", cancelLabel: "Cancel" },
  );
  if (!ok) return;

  // Local-only run: the backend just returns the result.
  const run: Run = { id: -Date.now(), dir, kind: "clean", status: "running", lines: [], planState: "none", ansi: new Map() };
  runs.set(run.id, run);
  selectRun(run.id);
  try {
    const done = await invoke<CleanSummary>("clean_caches", { dir });
    const lines = done.paths.map((p) => ({ stream: "stdout", line: `removed ${relName(p)}` }));
    lines.push(...done.errors.map((e) => ({ stream: "stderr", line: e })));
    run.lines.push(...lines, { stream: "gestalt", line: `Freed ${formatBytes(done.bytes)}.` });
    run.summary = done.paths.length ? `freed ${formatBytes(done.bytes)}` : "nothing removed";
    run.status = done.errors.length ? "failed" : "succeeded";
  } catch (e) {
    run.lines.push({ stream: "stderr", line: String(e) });
    run.status = "failed";
  }
  if (runs.has(run.id)) {
    if (run.id === selectedRunId) selectRun(run.id);
    else renderTabs();
  }
}

async function confirmAndApply(plan: Run) {
  if (plan.planState !== "ready") return;
  const summary = planSummary(plan) ?? "The plan has changes.";
  const ok = await ask(`Apply this plan to\n${plan.dir}?\n\n${summary}\n\nThis will change real infrastructure.`, {
    title: "Apply plan",
    kind: "warning",
    okLabel: "Apply",
    cancelLabel: "Cancel",
  });
  // Re-check: the tab may have been closed while the dialog was open.
  if (!ok || plan.planState !== "ready") return;

  // The apply continues this run: its output and exit arrive under plan.id.
  plan.planState = "applied";
  plan.kind = "apply";
  plan.status = "running";
  plan.exit = undefined;
  if (plan.id === selectedRunId) renderHeader();
  renderTabs();
  try {
    await invoke("apply_plan", { planRunId: plan.id });
  } catch (e) {
    const exit = { runId: plan.id, code: null, success: false, cancelled: false, hasChanges: null, error: String(e), started: false };
    handleExit(plan, exit);
  }
}

function handleExit(run: Run, e: ExitEvent) {
  run.exit = e;
  run.status = e.cancelled ? "cancelled" : e.success ? "succeeded" : "failed";
  if (e.error) run.lines.push({ stream: "stderr", line: e.error });
  if (run.planState === "applied") {
    // The apply step's exit. One that never started (folder busy, env
    // error...) leaves the plan applicable.
    if (!e.started) run.planState = "ready";
  } else if (e.hasChanges) {
    run.planState = "ready";
  }

  if (run.id === selectedRunId) selectRun(run.id);
  else renderTabs();

  if (run.applyWhenReady && run.planState === "ready") {
    run.applyWhenReady = false; // ask once; the Apply button covers retries
    selectRun(run.id);
    void confirmAndApply(run);
  }
}

// ---- Run rendering ---------------------------------------------------------

function renderTabs() {
  const tabs = $("run-tabs");
  tabs.replaceChildren(
    ...[...runs.values()].map((r) => {
      const tab = document.createElement("div");
      tab.className =
        `run-tab ${r.status}` + (r.planState === "ready" ? " plan-ready" : "") + (r.id === selectedRunId ? " selected" : "");
      tab.title = r.dir;
      tab.onclick = () => selectRun(r.id);
      // Middle-click closes, as in browser and editor tabs.
      tab.onauxclick = (e) => {
        if (e.button === 1 && r.status !== "running") closeRun(r);
      };

      const label = document.createElement("button");
      label.className = "run-tab-label";
      label.textContent = `${r.kind} · ${relName(r.dir)}`;

      const close = document.createElement("button");
      close.className = "run-tab-close";
      close.innerHTML = icons.x;
      close.title = "Close";
      close.setAttribute("aria-label", `Close ${r.kind} · ${relName(r.dir)}`);
      // A running tab can't be closed; Cancel it first.
      close.hidden = r.status === "running";
      close.onclick = (e) => {
        e.stopPropagation();
        closeRun(r);
      };

      // macOS puts a tab's close button on the left; Windows and Linux on the right.
      tab.append(...(IS_MAC ? [close, label] : [label, close]));
      return tab;
    }),
  );
}

/** Render one output line; tofu's ANSI colours carry over between lines of a stream. */
function lineNode(run: Run, l: Run["lines"][number]): Node {
  let state = run.ansi.get(l.stream);
  if (!state) run.ansi.set(l.stream, (state = newAnsiState()));
  const content = renderAnsi(l.line, state);
  content.appendChild(document.createTextNode("\n"));
  if (l.stream === "stdout") return content;
  const s = document.createElement("span");
  s.className = l.stream;
  s.appendChild(content);
  return s;
}

function renderHeader() {
  const run = selectedRunId != null ? runs.get(selectedRunId) : undefined;
  $("run-header").hidden = !run;
  if (!run) return;
  $("run-title").textContent = run.kind === "clean" ? `clean caches — ${run.dir}` : `tofu ${run.kind} — ${run.dir}`;
  const status = $("run-status");
  status.className = `status ${run.status}`;
  status.textContent = statusText(run);
  $<HTMLButtonElement>("run-apply").hidden = run.planState !== "ready";
  $<HTMLButtonElement>("run-cancel").hidden = run.status !== "running" || run.kind === "clean";
}

function selectRun(id: number | null) {
  selectedRunId = id;
  const out = $("run-output");
  const run = id != null ? runs.get(id) : undefined;
  if (run) run.ansi = new Map();
  out.replaceChildren(...(run?.lines.map((l) => lineNode(run, l)) ?? []));
  out.scrollTop = out.scrollHeight;
  renderHeader();
  renderTabs();
}

listen<OutputEvent>("tofu://output", ({ payload }) => {
  const entry = { stream: payload.stream, line: payload.line };
  const run = runs.get(payload.runId);
  if (!run) {
    const e = early.get(payload.runId) ?? { lines: [] };
    e.lines.push(entry);
    early.set(payload.runId, e);
    return;
  }
  run.lines.push(entry);
  if (run.id === selectedRunId) {
    const out = $("run-output");
    const atBottom = out.scrollHeight - out.scrollTop - out.clientHeight < 40;
    out.appendChild(lineNode(run, entry));
    if (atBottom) out.scrollTop = out.scrollHeight;
  }
});

listen<ExitEvent>("tofu://exit", ({ payload }) => {
  const run = runs.get(payload.runId);
  if (!run) {
    const e = early.get(payload.runId) ?? { lines: [] };
    e.exit = payload;
    early.set(payload.runId, e);
    return;
  }
  handleExit(run, payload);
});

$("run-apply").onclick = () => {
  const run = selectedRunId != null ? runs.get(selectedRunId) : undefined;
  if (run) void confirmAndApply(run);
};

$("run-cancel").onclick = () => {
  if (selectedRunId != null) invoke("cancel_tofu", { runId: selectedRunId }).catch(console.error);
};

function closeRun(run: Run) {
  if (run.planState === "ready") {
    run.planState = "discarded";
    invoke("discard_plan", { planRunId: run.id }).catch(console.error);
  }
  runs.delete(run.id);
  if (run.id === selectedRunId) {
    const remaining = [...runs.keys()];
    selectRun(remaining.length ? remaining[remaining.length - 1] : null);
  } else {
    renderTabs();
  }
}

// ---- Folder tree -------------------------------------------------------------

let currentRoot: string | null = null;
/** Every folder with cloud settings of its own, for tree badges. */
let cloudFolders: Record<string, Record<string, string>> = {};

/** Saving sends `cloud://changed` to every window, which refreshes what shows settings. */
const editCloud = (dir: string) => void openCloudSettings(dir, relName(dir), relName);

async function openFileEditor(path: string) {
  try {
    await invoke("open_file_editor", { path });
  } catch (e) {
    await message(String(e), { title: "Can't open file", kind: "error" });
  }
}

// Gestalt › Settings… (⌘,) in the app menu.
listen("menu://settings", () => openSettings());

// Plan from one of this window's file editor windows: run here and come
// forward to show it.
listen<PlanRequest>(PLAN_EVENT, ({ payload }) => {
  void startPlan(payload.dir);
  void appWindow.setFocus();
});

// File › Close (⌘W): the front dialog, else the window.
listen("menu://close", () => {
  if (!closeFrontDialog()) void appWindow.close();
});

// Workspace menu items, sent to the window in front.
listen("menu://clean-all", () => {
  if (currentRoot) void cleanCaches(currentRoot);
});
listen("menu://cloud-settings", () => {
  if (currentRoot) editCloud(currentRoot);
});
listen("cloud://changed", () => {
  if (!currentRoot) return;
  void renderTree();
  void refreshCloudStatus(currentRoot);
});

/** The toolbar button: cloud settings in effect for the open folder, `root`. */
async function refreshCloudStatus(root: string) {
  const btn = $<HTMLButtonElement>("cloud-status");
  btn.hidden = false;
  const settings = await invoke<CloudSetting[]>("cloud_effective", { dir: root }).catch((e) => {
    console.error(e);
    return [] as CloudSetting[];
  });
  if (root !== currentRoot) return;
  const summary = cloudSummary(settings);
  const parts = cloudParts(settings);
  btn.querySelector<HTMLElement>(".icon")!.hidden = parts.length > 0;
  $("cloud-status-text").replaceChildren(
    ...(parts.length
      ? parts.flatMap((p) => [providerLogo(p.id, p.title), p.text])
      : ["No cloud account"]),
  );
  btn.classList.toggle("unset", !summary);
  btn.title = summary
    ? `tofu runs here use ${summary}.\nFolders below can override it. Click to change.`
    : "No AWS profile, gcloud configuration or Azure subscription chosen for this folder. Click to choose one.";
}
$("cloud-status").onclick = () => {
  if (currentRoot) editCloud(currentRoot);
};

/**
 * Move a file or folder to the Trash, after asking. A symlink goes as a link:
 * what it points to stays.
 */
async function trashEntry(entry: DirEntry) {
  const what = entry.isDir ? `the folder "${entry.name}" and everything in it` : `"${entry.name}"`;
  const link = entry.isSymlink ? "\n\nIt's a symbolic link: only the link is removed, not what it points to." : "";
  const ok = await ask(`Move ${what} to the Trash?\n\n${relName(entry.path)}${link}`, {
    title: "Move to Trash",
    kind: "warning",
    okLabel: "Move to Trash",
    cancelLabel: "Cancel",
  });
  if (!ok) return;
  try {
    await invoke("move_to_trash", { path: entry.path });
  } catch (e) {
    await message(String(e), { title: "Move to Trash", kind: "error" });
    return;
  }
  if (selectedPath === entry.path) selectedPath = null;
  await renderTree();
}

/**
 * Right-click menu for `entry` (a folder, or a file showing its folder's
 * menu). tofu commands only for folders with a main.tf (isModule).
 */
async function showContextMenu(dir: string, isModule: boolean, entry: DirEntry) {
  const item = (text: string, action: () => void) => MenuItem.new({ text, action });
  const tofuItems = isModule
    ? [
        await item("Import…", () => void startImport(dir)),
        await item("Plan", () => void startPlan(dir)),
        // Plans first, then asks to apply that exact saved plan.
        await item("Apply…", () => void startPlan(dir, true)),
        await PredefinedMenuItem.new({ item: "Separator" }),
      ]
    : [];
  // Anything but the opened folder itself, named so a file's entry isn't
  // mistaken for its folder's.
  const trashItems =
    entry.path === currentRoot
      ? []
      : [await PredefinedMenuItem.new({ item: "Separator" }), await item(`Move "${entry.name}" to Trash…`, () => void trashEntry(entry))];
  const menu = await Menu.new({
    items: [
      ...tofuItems,
      // Any folder: parent folders hold shared settings for the modules below.
      await item("Cloud Settings…", () => editCloud(dir)),
      // Any folder: this folder and everything below it.
      await item("Clean .terraform Caches…", () => void cleanCaches(dir)),
      ...trashItems,
    ],
  });
  await menu.popup();
}

/** Folder paths currently expanded; survives refreshes. */
const expanded = new Set<string>();
let selectedPath: string | null = null;
/** Bumped on every (re)build so a slow, superseded build doesn't win. */
let treeGeneration = 0;

function selectRow(row: HTMLElement, path: string) {
  document.querySelectorAll(".tree-row.selected").forEach((el) => el.classList.remove("selected"));
  row.classList.add("selected");
  selectedPath = path;
}

/** A link icon after the label of a symlinked file or folder. */
function linkBadge(entry: DirEntry): HTMLElement[] {
  if (!entry.isSymlink) return [];
  const badge = document.createElement("span");
  badge.className = "link-badge";
  badge.innerHTML = icons.link;
  badge.title = "Symbolic link: changes affect everywhere it's linked from";
  return [badge];
}

/** A file row. Right-clicking it offers the run menu for its folder. */
function buildFileNode(entry: DirEntry, depth: number, parent: DirEntry): HTMLLIElement {
  const li = document.createElement("li");
  li.setAttribute("role", "treeitem");
  const kind = fileKind(entry.name);

  const row = document.createElement("div");
  row.className = `tree-row file file-${kind}` + (entry.path === selectedPath ? " selected" : "");
  row.style.paddingLeft = `${depth * 14 + 6}px`;

  const caret = document.createElement("span");
  caret.className = "caret";
  const icon = document.createElement("span");
  icon.className = "icon";
  icon.innerHTML = fileIcon(kind);
  const label = document.createElement("span");
  label.className = "label";
  label.textContent = entry.name;
  row.append(caret, icon, label, ...linkBadge(entry));

  row.onclick = () => selectRow(row, entry.path);
  row.title = `${entry.path}\nDouble-click to edit`;
  // Files open in their own editor window (or bring it forward).
  row.ondblclick = () => void openFileEditor(entry.path);
  row.oncontextmenu = (ev) => {
    ev.preventDefault();
    ev.stopPropagation();
    selectRow(row, entry.path);
    void showContextMenu(parent.path, parent.isModule, entry);
  };

  li.append(row);
  return li;
}

/** Build a tree node, eagerly loading children of folders in `expanded`. */
async function buildNode(entry: DirEntry, depth: number, parent?: DirEntry): Promise<HTMLLIElement> {
  if (!entry.isDir && parent) return buildFileNode(entry, depth, parent);

  const li = document.createElement("li");
  li.setAttribute("role", "treeitem");

  const row = document.createElement("div");
  row.className = "tree-row folder" + (entry.isModule ? " module" : "") + (entry.path === selectedPath ? " selected" : "");
  row.style.paddingLeft = `${depth * 14 + 6}px`;
  row.title = entry.path;

  const caret = document.createElement("span");
  caret.className = "caret";
  caret.textContent = entry.hasChildren ? "▸" : "";

  const icon = document.createElement("span");
  icon.className = "icon";
  icon.innerHTML = icons.folder;

  const label = document.createElement("span");
  label.className = "label";
  label.textContent = entry.name;

  row.append(caret, icon, label, ...linkBadge(entry));
  if (entry.isModule) {
    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = "tf";
    row.append(badge);
  }
  for (const b of cloudBadges(cloudFolders[entry.path] ?? {})) {
    row.append(providerLogo(b.id, `${b.title}\nApplies here and to all subfolders.`));
  }

  const children = document.createElement("ul");
  children.className = "tree";
  children.hidden = true;
  let loaded = false;

  const loadChildren = async () => {
    const kids = await invoke<DirEntry[]>("list_dir", { path: entry.path });
    children.replaceChildren(...(await Promise.all(kids.map((k) => buildNode(k, depth + 1, entry)))));
    loaded = true;
  };
  const setOpen = (open: boolean) => {
    children.hidden = !open;
    caret.textContent = open ? "▾" : "▸";
    icon.innerHTML = open ? icons.folderOpen : icons.folder;
    if (open) expanded.add(entry.path);
    else expanded.delete(entry.path);
  };

  row.onclick = async () => {
    selectRow(row, entry.path);
    if (!entry.hasChildren) return;
    const opening = children.hidden !== false;
    if (opening && !loaded) {
      try {
        await loadChildren();
      } catch (e) {
        console.error(e);
        return;
      }
    }
    setOpen(opening);
  };
  row.oncontextmenu = (ev) => {
    ev.preventDefault();
    ev.stopPropagation();
    selectRow(row, entry.path);
    void showContextMenu(entry.path, entry.isModule, entry);
  };

  if (entry.hasChildren && expanded.has(entry.path)) {
    try {
      await loadChildren();
      setOpen(true);
    } catch {
      expanded.delete(entry.path); // folder vanished or became unreadable
    }
  }

  li.append(row, children);
  return li;
}

/** (Re)build the whole tree from disk, keeping expansion and selection. */
async function renderTree() {
  if (!currentRoot) return;
  const gen = ++treeGeneration;
  const btn = $<HTMLButtonElement>("refresh-tree");
  btn.disabled = true;
  try {
    // open_root also tells the backend which folder to watch for this window.
    const entry = await invoke<DirEntry>("open_root", { path: currentRoot });
    cloudFolders = await invoke<typeof cloudFolders>("cloud_folders").catch((e) => {
      console.error(e);
      return {};
    });
    const root = await buildNode(entry, 0);
    if (gen !== treeGeneration) return;
    $("tree").replaceChildren(root);
    showWelcome(false);
  } catch (e) {
    if (gen !== treeGeneration) return;
    $("tree").replaceChildren();
    showWelcome(true, `Could not open ${currentRoot}: ${e}`);
  } finally {
    if (gen === treeGeneration) btn.disabled = false;
  }
}

async function loadRoot(path: string) {
  currentRoot = path;
  try {
    localStorage.setItem(LAST_ROOT_KEY, path);
  } catch {
    /* storage unavailable */
  }
  $("root-path").textContent = path;
  $("root-path").title = "";
  appWindow.setTitle(`${baseName(path)} — Gestalt`).catch(console.error);
  expanded.clear();
  expanded.add(path); // start with the root expanded
  selectedPath = null;
  await renderTree();
  void resetCommits(path);
  void refreshCloudStatus(path);
  renderTabs();
}

interface WorkspaceChanges {
  worktree: boolean;
  structure: boolean;
  folders: boolean;
  gitIndex: boolean;
  gitRefs: boolean;
}

// The backend watches the open folder and its git metadata (debounced) and
// says what kind of thing changed; refresh only the affected views.
listen<WorkspaceChanges>("workspace://changed", ({ payload: c }) => {
  if (!currentRoot) return;
  if (c.structure) void renderTree();
  // New commits/branch, or folders appearing/disappearing (commit rows only
  // list folders that match the local checkout): reload the whole list.
  if (c.gitRefs || c.folders) void refreshCommits();
  else if (c.worktree || c.gitIndex) void refreshUncommitted();
});

function refreshAll() {
  void renderTree();
  void refreshCommits();
}

$("refresh-tree").onclick = refreshAll;

// ⌘R / Ctrl+R / F5 would reload the webview and lose all runs; refresh instead.
document.addEventListener("keydown", (ev) => {
  if (ev.key === "F5" || ((ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === "r")) {
    ev.preventDefault();
    refreshAll();
  }
});

/** No folder open (or it failed to open): show the welcome panel instead of the workspace. */
function showWelcome(show: boolean, error?: string) {
  $("welcome").hidden = !show;
  $("workspace").hidden = show;
  const err = $("welcome-error");
  err.textContent = error ?? "";
  err.hidden = !error;
}

async function pickRoot() {
  const picked = await open({
    directory: true,
    multiple: false,
    title: "Open infrastructure folder",
    defaultPath: currentRoot ?? undefined,
  });
  if (typeof picked === "string") await loadRoot(picked);
}

$("open-root").onclick = () => void pickRoot();
$("welcome-open").onclick = () => void pickRoot();

// ⌘O / Ctrl+O opens a folder.
document.addEventListener("keydown", (ev) => {
  if ((ev.metaKey || ev.ctrlKey) && ev.key.toLowerCase() === "o") {
    ev.preventDefault();
    void pickRoot();
  }
});

// Suppress the webview's default context menu everywhere except text inputs.
document.addEventListener("contextmenu", (ev) => {
  if (!(ev.target instanceof HTMLInputElement || ev.target instanceof HTMLTextAreaElement)) ev.preventDefault();
});

// ---- Startup -----------------------------------------------------------------

interface EnvStatus {
  shell: string | null;
  varCount: number;
  error: string | null;
}

function renderEnvStatus(s: EnvStatus) {
  const el = $("env-status");
  el.classList.toggle("error", !!s.error);
  if (s.error) {
    el.textContent = "shell env not loaded";
    el.title = `${s.error}\n\ntofu will run with the app's default environment.`;
  } else {
    el.textContent = `env: ${s.shell ? baseName(s.shell) : "shell"} (${s.varCount} vars)`;
    el.title = `tofu runs with the environment from your login shell (${s.shell}).\nClick "Reload env" after changing your shell config.`;
  }
}

function refreshTofuVersion() {
  const el = $("tofu-version");
  invoke<string>("tofu_version")
    .then((v) => {
      el.textContent = v;
      el.title = "";
      el.classList.remove("error");
    })
    .catch((e) => {
      el.textContent = "tofu not found";
      el.title = String(e);
      el.classList.add("error");
    });
}

$("env-reload").onclick = async () => {
  const btn = $<HTMLButtonElement>("env-reload");
  btn.disabled = true;
  $("env-status").textContent = "loading env…";
  try {
    renderEnvStatus(await invoke<EnvStatus>("reload_shell_env"));
    refreshTofuVersion();
  } finally {
    btn.disabled = false;
  }
};

$("env-status").textContent = "loading env…";
invoke<EnvStatus>("shell_env_status").then(renderEnvStatus).catch(console.error);
refreshTofuVersion();

// Closing a window cancels its runs: check first. Then it closes its file
// editor windows, so their changes are saved (or the user is asked); it
// stays open if one does.
appWindow
  .onCloseRequested(async (ev) => {
    const running = [...runs.values()].filter((r) => r.status === "running" && r.kind !== "clean").length;
    if (running) {
      const ok = await ask(`${running} tofu run${running > 1 ? "s are" : " is"} still running in this window. Cancel and close?`, {
        title: "Close window",
        kind: "warning",
        okLabel: "Cancel runs and close",
        cancelLabel: "Keep open",
      });
      if (!ok) return ev.preventDefault();
    }
    if (!(await closeEditorWindows(appWindow.label))) ev.preventDefault();
  })
  .catch(console.error);

/**
 * Startup folder: the one this window was opened for (command line /
 * GESTALT_ROOT for the launch window), else the last one opened.
 */
async function openInitialRoot() {
  const configured = await invoke<{ path: string; source: string } | null>("initial_root").catch((e) => {
    console.error(e);
    return null;
  });
  if (configured) {
    await loadRoot(configured.path);
    $("root-path").title = `Opened from ${configured.source}`;
    return;
  }
  let last: string | null = null;
  try {
    // Only the launch window: a new window without a folder starts empty.
    if (IS_LAUNCH_WINDOW) last = localStorage.getItem(LAST_ROOT_KEY);
  } catch {
    /* storage unavailable */
  }
  if (last) await loadRoot(last);
  else showWelcome(true);
}

// Fill static icon placeholders in the HTML.
document.querySelectorAll<HTMLElement>("[data-icon]").forEach((el) => {
  el.innerHTML = icons[el.dataset.icon as keyof typeof icons] ?? "";
});

initSystemColors();
initSidebar(
  (dir) => void startPlan(dir),
  (dir) => void startPlan(dir, true),
);
void openInitialRoot();
