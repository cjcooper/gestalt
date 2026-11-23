import { invoke } from "@tauri-apps/api/core";
import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { ask, message, open } from "@tauri-apps/plugin-dialog";
import { newAnsiState, renderAnsi, stripAnsi } from "./ansi";
import { cloudBadges, openCloudSettings, providerLogo, type OwnSettings } from "./cloudSettings";
import { fileIcon, fileKind, icons } from "./icons";
import { openSettings } from "./settings";
import { PLAN_EVENT, closeEditorWindows, closeFrontDialog, type PlanRequest } from "./windows";
import { openImportDialog, type ImportRequest } from "./importDialog";
import { initSystemColors } from "./systemColors";
import { initWindowChrome } from "./windowChrome";
import { initSidebar, refreshCommits, refreshUncommitted, resetCommits, showFiles } from "./sidebar";
import { initFileFind } from "./fileFind";
import { createTextFind } from "./textFind";
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

/** Add a run's tab, or with `replaces`, put it in that finished run's tab. */
function registerRun(run: Run, replaces?: number) {
  const pending = early.get(run.id);
  early.delete(run.id);
  if (pending) run.lines.push(...pending.lines);
  const old = replaces != null ? runs.get(replaces) : undefined;
  // A tab that started running again meanwhile (its plan was applied) keeps its run.
  if (old && old.status !== "running") {
    discardPlan(old);
    const entries = [...runs].map(([id, r]): [number, Run] => (id === replaces ? [run.id, run] : [id, r]));
    runs.clear();
    for (const [id, r] of entries) runs.set(id, r);
  } else {
    runs.set(run.id, run);
  }
  selectRun(run.id);
  if (pending?.exit) handleExit(run, pending.exit);
}

/**
 * Start a backend run and give it a tab. The backend reports everything,
 * including failures to start, as output/exit events, so it all shows in the
 * run's pane. Only a failed invoke itself (shouldn't happen) is handled here.
 */
async function startRun(
  dir: string,
  kind: RunKind,
  command: string,
  args: Record<string, unknown>,
  extra: Partial<Run> = {},
  replaces?: number,
) {
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
  registerRun({ id, dir, kind, status: "running", lines: [], planState: "none", ansi: new Map(), ...extra }, replaces);
}

/**
 * Plan in `dir`. With `applyWhenReady`, the run is an apply: init,
 * plan, confirm, then apply that saved plan, all in this one run's pane.
 */
const startPlan = (dir: string, applyWhenReady = false, replaces?: number) =>
  startRun(dir, applyWhenReady ? "apply" : "plan", "run_plan", { dir }, { applyWhenReady }, replaces);

/** Plan/Apply asked for while the folder was busy, by folder; an apply covers a plan. */
const queued = new Map<string, { apply: boolean }>();
/** Folders whose Plan/Apply is being started (no run id yet). */
const starting = new Set<string>();
/** Runs whose "Apply this plan?" question is open. */
const confirming = new Set<number>();

/** The folder's plan/apply tab (the newest, if there are several). */
function folderTab(dir: string): Run | undefined {
  return [...runs.values()].reverse().find((r) => r.dir === dir && (r.kind === "plan" || r.kind === "apply"));
}

/** Something is running in the folder, or its tab is waiting on the apply question. */
function folderBusy(dir: string): boolean {
  if (starting.has(dir)) return true;
  return [...runs.values()].some((r) => r.dir === dir && (r.status === "running" || confirming.has(r.id)));
}

/**
 * Plan (or apply) `dir` from the tree, a commit's folder list or an editor.
 * The folder keeps one tab: the action reuses it, waiting for whatever is
 * running there to finish first.
 */
function requestPlan(dir: string, apply = false) {
  if (folderBusy(dir)) {
    queued.set(dir, { apply: apply || !!queued.get(dir)?.apply });
    const tab = folderTab(dir);
    if (tab) selectRun(tab.id);
    return;
  }
  starting.add(dir);
  void startPlan(dir, apply, folderTab(dir)?.id).finally(() => {
    starting.delete(dir);
    runQueued(dir);
  });
}

/** Start the folder's waiting action once nothing else is going on there. */
function runQueued(dir: string) {
  const next = queued.get(dir);
  if (!next || folderBusy(dir)) return;
  queued.delete(dir);
  requestPlan(dir, next.apply);
}

/** A failed or cancelled plan/apply can be planned again in its own tab. */
const canReplan = (run: Run) => (run.kind === "plan" || run.kind === "apply") && (run.status === "failed" || run.status === "cancelled");

/** Plan again in a failed run's tab; an apply asks to apply again once planned. */
function replan(run: Run) {
  if (!canReplan(run)) return;
  void startPlan(run.dir, run.kind === "apply", run.id);
}

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
  if (plan.planState !== "ready" || confirming.has(plan.id)) return;
  const summary = planSummary(plan) ?? "The plan has changes.";
  // Plan/Apply asked for in this folder meanwhile waits for the answer.
  confirming.add(plan.id);
  let ok: boolean;
  try {
    ok = await ask(`Apply this plan to\n${plan.dir}?\n\n${summary}\n\nThis will change real infrastructure.`, {
      title: "Apply plan",
      kind: "warning",
      okLabel: "Apply",
      cancelLabel: "Cancel",
    });
  } finally {
    confirming.delete(plan.id);
  }
  // Re-check: the tab may have been closed or replaced while the dialog was open.
  if (!ok || plan.planState !== "ready") {
    runQueued(plan.dir);
    return;
  }

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
  runQueued(run.dir);
}

// ---- Run rendering ---------------------------------------------------------

/** ⌘F: find in the shown run's output. */
const outputFind = createTextFind($("run-find"), $("run-output"));

function renderTabs() {
  const tabs = $("run-tabs");
  tabs.replaceChildren(
    ...[...runs.values()].map((r) => {
      const tab = document.createElement("div");
      tab.className =
        `run-tab ${r.status}` + (r.planState === "ready" ? " plan-ready" : "") + (r.id === selectedRunId ? " selected" : "");
      tab.title = r.dir;
      tab.onclick = () => selectRun(r.id);
      tab.oncontextmenu = (e) => {
        e.preventDefault();
        selectRun(r.id);
        void showTabMenu(r);
      };
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
  const waiting = run === folderTab(run.dir) ? queued.get(run.dir) : undefined;
  status.textContent = statusText(run) + (waiting ? ` · ${waiting.apply ? "apply" : "plan"} queued` : "");
  $<HTMLButtonElement>("run-replan").hidden = !canReplan(run);
  $<HTMLButtonElement>("run-apply").hidden = run.planState !== "ready";
  $<HTMLButtonElement>("run-cancel").hidden = run.status !== "running" || run.kind === "clean";
}

function selectRun(id: number | null) {
  selectedRunId = id;
  const out = $("run-output");
  const run = id != null ? runs.get(id) : undefined;
  if (run) run.ansi = new Map();
  out.replaceChildren(...(run?.lines.map((l) => lineNode(run, l)) ?? []));
  outputFind.refresh();
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
    const node = lineNode(run, entry);
    // A plain line is a fragment, emptied by appending it.
    const added = node instanceof DocumentFragment ? [...node.childNodes] : [node];
    out.appendChild(node);
    outputFind.appended(added);
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

$("run-replan").onclick = () => {
  const run = selectedRunId != null ? runs.get(selectedRunId) : undefined;
  if (run) replan(run);
};

function cancelRun(run: Run) {
  // Cancelling stops the folder's waiting Plan/Apply too.
  queued.delete(run.dir);
  renderHeader();
  invoke("cancel_tofu", { runId: run.id }).catch(console.error);
}

$("run-cancel").onclick = () => {
  const run = selectedRunId != null ? runs.get(selectedRunId) : undefined;
  if (run) cancelRun(run);
};

/** Drop a run's saved plan, if it has one waiting to be applied. */
function discardPlan(run: Run) {
  if (run.planState !== "ready") return;
  run.planState = "discarded";
  invoke("discard_plan", { planRunId: run.id }).catch(console.error);
}

function closeRun(run: Run) {
  discardPlan(run);
  runs.delete(run.id);
  if (run.id === selectedRunId) {
    const remaining = [...runs.keys()];
    selectRun(remaining.length ? remaining[remaining.length - 1] : null);
  } else {
    renderTabs();
  }
}

// ---- Copying and right-click menus ---------------------------------------------

async function copyText(text: string) {
  try {
    await writeText(text);
  } catch (e) {
    await message(String(e), { title: "Copy", kind: "error" });
  }
}

const menuItem = (text: string, action: () => void, enabled = true, accelerator?: string) =>
  MenuItem.new({ text, action, enabled, accelerator });
const separator = () => PredefinedMenuItem.new({ item: "Separator" });

/** What's selected in the output, if the selection is there. */
function outputSelection(): string {
  const sel = document.getSelection()!;
  return !sel.isCollapsed && $("run-output").contains(sel.anchorNode) ? sel.toString() : "";
}

function selectAllOutput() {
  const range = document.createRange();
  range.selectNodeContents($("run-output"));
  const sel = document.getSelection()!;
  sel.removeAllRanges();
  sel.addRange(range);
}

/** The run's output as plain text, without tofu's colour codes. */
const outputText = (run: Run) => run.lines.map((l) => `${stripAnsi(l.line)}\n`).join("");

/** The run's own buttons (Plan again, Apply, Cancel) as menu items, as the header shows them. */
async function runActionItems(run: Run) {
  const items = [];
  if (canReplan(run)) items.push(await menuItem("Plan Again", () => replan(run)));
  if (run.planState === "ready") items.push(await menuItem("Apply This Plan…", () => void confirmAndApply(run)));
  if (run.status === "running" && run.kind !== "clean") items.push(await menuItem("Cancel Run", () => cancelRun(run)));
  return items.length ? [...items, await separator()] : [];
}

/** Right-click in the output: copy, find, and the run's own actions. */
async function showOutputMenu(run: Run) {
  const selected = outputSelection();
  const menu = await Menu.new({
    items: [
      await menuItem("Copy", () => void copyText(selected), !!selected, "CmdOrCtrl+C"),
      await menuItem("Copy All Output", () => void copyText(outputText(run)), run.lines.length > 0),
      await menuItem("Select All", selectAllOutput),
      await separator(),
      await menuItem("Find…", () => outputFind.open(), true, "CmdOrCtrl+F"),
      await separator(),
      ...(await runActionItems(run)),
      await menuItem("Copy Folder Path", () => void copyText(run.dir)),
    ],
  });
  await menu.popup();
}

/** Right-click on a run's tab. */
async function showTabMenu(run: Run) {
  const others = [...runs.values()].filter((r) => r !== run && r.status !== "running");
  const menu = await Menu.new({
    items: [
      ...(await runActionItems(run)),
      await menuItem("Copy Folder Path", () => void copyText(run.dir)),
      await menuItem("Show Folder in Files", () => void reveal(run.dir)),
      await separator(),
      // A running tab can't be closed; Cancel it first.
      await menuItem("Close Tab", () => closeRun(run), run.status !== "running"),
      await menuItem("Close Other Tabs", () => others.forEach(closeRun), others.length > 0),
    ],
  });
  await menu.popup();
}

$("run-output").addEventListener("contextmenu", (ev) => {
  ev.preventDefault();
  const run = selectedRunId != null ? runs.get(selectedRunId) : undefined;
  if (run) void showOutputMenu(run);
});

// ---- Folder tree -------------------------------------------------------------

let currentRoot: string | null = null;
/** Every folder with cloud settings of its own, as paths in this workspace, for tree badges. */
let cloudFolders: Record<string, OwnSettings> = {};

/** Saving sends `cloud://changed` to every window, which refreshes what shows settings. */
const editCloud = (dir: string) => void openCloudSettings(dir, relName(dir), relName);

async function openFileEditor(path: string, line?: number) {
  try {
    await invoke("open_file_editor", { path, line });
  } catch (e) {
    await message(String(e), { title: "Can't open file", kind: "error" });
  }
}

// Gestalt › Settings… (⌘,) in the app menu.
listen("menu://settings", () => openSettings());

// Plan from one of this window's file editor windows: run here and come
// forward to show it.
listen<PlanRequest>(PLAN_EVENT, ({ payload }) => {
  requestPlan(payload.dir);
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
listen("cloud://changed", () => {
  if (currentRoot) void renderTree();
});

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
  const openItems = entry.isDir ? [] : [await item("Open", () => void openFileEditor(entry.path)), await separator()];
  const tofuItems = isModule
    ? [
        await item("Import…", () => void startImport(dir)),
        await item("Plan", () => requestPlan(dir)),
        // Plans first, then asks to apply that exact saved plan.
        await item("Apply…", () => requestPlan(dir, true)),
        await PredefinedMenuItem.new({ item: "Separator" }),
      ]
    : [];
  // Anything but the opened folder itself, named so a file's entry isn't
  // mistaken for its folder's.
  const trashItems =
    entry.path === currentRoot
      ? []
      : [await PredefinedMenuItem.new({ item: "Separator" }), await item(`Move "${entry.name}" to Trash…`, () => void trashEntry(entry))];
  const rel = entry.path === currentRoot ? "." : relName(entry.path);
  const menu = await Menu.new({
    items: [
      ...openItems,
      ...tofuItems,
      // Any folder below the opened one: parent folders hold shared settings
      // for the modules below, but an account on the whole tree would reach
      // every module in it (the backend refuses a repository's top too).
      ...(dir === currentRoot ? [] : [await item("Cloud Settings…", () => editCloud(dir))]),
      // Any folder: this folder and everything below it.
      await item("Clean .terraform Caches…", () => void cleanCaches(dir)),
      await separator(),
      await item("Copy Path", () => void copyText(entry.path)),
      await item("Copy Relative Path", () => void copyText(rel)),
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

/** The shortcut arrow drawn over the icon of a symlinked file or folder. */
function linkBadge(entry: DirEntry): HTMLElement[] {
  if (!entry.isSymlink) return [];
  const badge = document.createElement("span");
  badge.className = "link-badge";
  badge.innerHTML = icons.linkArrow;
  badge.title = "Symbolic link: changes affect everywhere it's linked from";
  return [badge];
}

/**
 * The system's icon for a file, one lookup per file type (extension, or the
 * whole name when there is none). A symlink is looked up on its own: it gets
 * its target's icon, which may differ from what its name suggests.
 */
const osIcons = new Map<string, Promise<string | null>>();
function osIcon(entry: DirEntry): Promise<string | null> {
  const lookup = () => invoke<string | null>("file_icon", { path: entry.path }).catch(() => null);
  if (entry.isSymlink) return lookup();
  const dot = entry.name.lastIndexOf(".");
  const key = dot > 0 ? entry.name.slice(dot).toLowerCase() : entry.name;
  let icon = osIcons.get(key);
  if (!icon) osIcons.set(key, (icon = lookup()));
  return icon;
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
  icon.append(...linkBadge(entry));
  // OpenTofu files keep their own icons (the system's are usually blank
  // pages); the rest swap to what Finder shows once it arrives.
  if (kind !== "tofu" && kind !== "vars") {
    void osIcon(entry).then((url) => {
      if (!url) return;
      const img = document.createElement("img");
      img.className = "os-icon";
      img.src = url;
      img.alt = "";
      icon.firstElementChild!.replaceWith(img);
    });
  }
  const label = document.createElement("span");
  label.className = "label";
  label.textContent = entry.name;
  row.append(caret, icon, label);

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
  icon.append(...linkBadge(entry));

  const label = document.createElement("span");
  label.className = "label";
  label.textContent = entry.name;

  row.append(caret, icon, label);
  if (entry.isModule) {
    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = "tf";
    row.append(badge);
  }
  for (const b of cloudBadges(cloudFolders[entry.path] ?? { shared: {}, workspace: {} })) {
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
    // Swap only the folder glyph, keeping any symlink arrow drawn over it.
    icon.firstElementChild!.outerHTML = open ? icons.folderOpen : icons.folder;
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
    cloudFolders = await invoke<typeof cloudFolders>("cloud_folders", { root: currentRoot }).catch((e) => {
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

const parentDir = (p: string) => p.slice(0, Math.max(p.lastIndexOf("/"), p.lastIndexOf("\\")));

/** Show `path` (in the opened folder) in the tree: its folders open, it selected. */
async function reveal(path: string) {
  const root = currentRoot;
  if (!root || !path.startsWith(root)) return;
  showFiles();
  for (let p = parentDir(path); p.length >= root.length; p = parentDir(p)) expanded.add(p);
  selectedPath = path;
  await renderTree();
  document.querySelector("#tree .tree-row.selected")?.scrollIntoView({ block: "center" });
}

const fileFind = initFileFind({
  root: () => currentRoot,
  open: (path, line) => void openFileEditor(path, line),
  reveal: (path) => void reveal(path),
  showFiles,
});

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
  // The native title is hidden on macOS; the page's title bar names the folder.
  $("window-title").textContent = baseName(path);
  expanded.clear();
  expanded.add(path); // start with the root expanded
  selectedPath = null;
  fileFind.reset();
  await renderTree();
  void resetCommits(path);
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
  if (c.structure) {
    void renderTree();
    fileFind.invalidate();
  }
  // New commits/branch, or folders appearing/disappearing (commit rows only
  // list folders that match the local checkout): reload the whole list.
  if (c.gitRefs || c.folders) void refreshCommits();
  else if (c.worktree || c.gitIndex) void refreshUncommitted();
});

function refreshAll() {
  void renderTree();
  fileFind.invalidate();
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

// ⌘F finds in the run's output (⌘G / ⇧⌘G step through it), ⇧⌘F finds in
// files and ⌘P goes to a file.
document.addEventListener("keydown", (ev) => {
  if (!(ev.metaKey || ev.ctrlKey) || ev.altKey || $("workspace").hidden) return;
  const key = ev.key.toLowerCase();
  if (key === "f") {
    if (ev.shiftKey) fileFind.focus("text");
    else outputFind.open();
  } else if (key === "p" && !ev.shiftKey) {
    fileFind.focus("name");
  } else if (key === "g" && outputFind.isOpen()) {
    outputFind.step(!ev.shiftKey);
  } else {
    return;
  }
  ev.preventDefault();
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
void initWindowChrome();
initSidebar(
  (dir) => requestPlan(dir),
  (dir) => requestPlan(dir, true),
);
void openInitialRoot();
