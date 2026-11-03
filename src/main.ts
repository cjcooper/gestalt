import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { ask, open } from "@tauri-apps/plugin-dialog";
import { fileIcon, fileKind, icons } from "./icons";
import { openEnvEditor } from "./envEditor";
import { openImportDialog, type ImportRequest } from "./importDialog";
import { initSystemColors } from "./systemColors";
import { initSidebar, refreshCommits, refreshUncommitted, resetCommits } from "./sidebar";

// ---- Backend types ---------------------------------------------------------

type RunKind = "plan" | "apply" | "import";

interface DirEntry {
  name: string;
  path: string;
  isDir: boolean;
  hasChildren: boolean;
  isModule: boolean;
  hasEnvFile: boolean;
}

interface OutputEvent {
  runId: number;
  /** "gestalt" lines are the app's own notes, e.g. which .gestalt.env files applied. */
  stream: "stdout" | "stderr" | "gestalt";
  line: string;
}

interface ExitEvent {
  runId: number;
  code: number | null;
  success: boolean;
  cancelled: boolean;
  /** Plan runs only: a saved plan with changes is ready to apply. */
  hasChanges: boolean | null;
  error: string | null;
  /** Whether the requested command itself ran (vs. failing during setup). */
  started: boolean;
}

// ---- Run state ---------------------------------------------------------------

type RunStatus = "running" | "succeeded" | "failed" | "cancelled";

/** Lifecycle of a plan run's saved plan file. */
type PlanState = "none" | "ready" | "applied" | "discarded";

interface Run {
  id: number;
  dir: string;
  kind: RunKind;
  status: RunStatus;
  lines: { stream: string; line: string }[];
  exit?: ExitEvent;
  planState: PlanState;
  /** Started from "Apply…": prompt to apply once the plan finishes. */
  applyWhenReady?: boolean;
}

const runs = new Map<number, Run>();
let selectedRunId: number | null = null;
// Output/exit events can arrive before the invoke resolves with the run id.
const early = new Map<number, { lines: Run["lines"]; exit?: ExitEvent }>();

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
const LAST_ROOT_KEY = "gestalt.lastRoot";

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

/** The "Plan: 1 to add, 0 to change, 0 to destroy." line, if tofu printed one. */
function planSummary(run: Run): string | undefined {
  for (let i = run.lines.length - 1; i >= 0; i--) {
    const l = run.lines[i].line.trim();
    if (/^Plan: \d+ to /.test(l)) return l;
  }
  return undefined;
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
  registerRun({ id, dir, kind, status: "running", lines: [], planState: "none", ...extra });
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

function statusText(run: Run): string {
  if (run.status === "running") return run.planState === "applied" ? "applying…" : "running…";
  if (run.planState === "applied" && run.status === "succeeded") {
    const s = planSummary(run);
    return s ? `applied — ${s}` : "applied";
  }
  // Only a plan's exit carries hasChanges.
  if (run.exit?.hasChanges != null && run.status === "succeeded") {
    if (!run.exit?.hasChanges) return "no changes";
    const s = planSummary(run) ?? "changes pending";
    return run.planState === "ready" ? (run.kind === "apply" ? `${s} (not applied)` : s) : `${s} (discarded)`;
  }
  return run.exit?.code != null && run.status === "failed" ? `failed (exit ${run.exit.code})` : run.status;
}

function renderTabs() {
  const tabs = $("run-tabs");
  tabs.replaceChildren(
    ...[...runs.values()].map((r) => {
      const b = document.createElement("button");
      b.className =
        `run-tab ${r.status}` + (r.planState === "ready" ? " plan-ready" : "") + (r.id === selectedRunId ? " selected" : "");
      b.textContent = `${r.kind} · ${relName(r.dir)}`;
      b.title = r.dir;
      b.onclick = () => selectRun(r.id);
      return b;
    }),
  );
}

function lineNode(l: Run["lines"][number]): Node {
  if (l.stream !== "stdout") {
    const s = document.createElement("span");
    s.className = l.stream;
    s.textContent = l.line + "\n";
    return s;
  }
  return document.createTextNode(l.line + "\n");
}

function renderHeader() {
  const run = selectedRunId != null ? runs.get(selectedRunId) : undefined;
  $("run-header").hidden = !run;
  if (!run) return;
  $("run-title").textContent = `tofu ${run.kind} — ${run.dir}`;
  const status = $("run-status");
  status.className = `status ${run.status}`;
  status.textContent = statusText(run);
  $<HTMLButtonElement>("run-apply").hidden = run.planState !== "ready";
  $<HTMLButtonElement>("run-cancel").hidden = run.status !== "running";
  $<HTMLButtonElement>("run-close").hidden = run.status === "running";
}

function selectRun(id: number | null) {
  selectedRunId = id;
  const out = $("run-output");
  const run = id != null ? runs.get(id) : undefined;
  out.replaceChildren(...(run?.lines.map(lineNode) ?? []));
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
    out.appendChild(lineNode(entry));
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

$("run-close").onclick = () => {
  const run = selectedRunId != null ? runs.get(selectedRunId) : undefined;
  if (!run) return;
  if (run.planState === "ready") {
    run.planState = "discarded";
    invoke("discard_plan", { planRunId: run.id }).catch(console.error);
  }
  runs.delete(run.id);
  const remaining = [...runs.keys()];
  selectRun(remaining.length ? remaining[remaining.length - 1] : null);
};

// ---- Folder tree -------------------------------------------------------------

let currentRoot: string | null = null;

async function editEnv(dir: string) {
  if (await openEnvEditor(dir)) await renderTree();
}

/** Folder menu. tofu commands only for folders with a main.tf (isModule). */
async function showContextMenu(dir: string, isModule: boolean) {
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
  const menu = await Menu.new({
    items: [
      ...tofuItems,
      // Any folder: parent folders hold shared settings for the modules below.
      await item("Edit .gestalt.env…", () => void editEnv(dir)),
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

/** A file row. Right-clicking it offers the run menu for its folder. */
function buildFileNode(entry: DirEntry, depth: number, parent: DirEntry): HTMLLIElement {
  const li = document.createElement("li");
  li.setAttribute("role", "treeitem");
  const kind = fileKind(entry.name);

  const row = document.createElement("div");
  row.className = `tree-row file file-${kind}` + (entry.path === selectedPath ? " selected" : "");
  row.style.paddingLeft = `${depth * 14 + 6}px`;
  row.title = entry.path;

  const caret = document.createElement("span");
  caret.className = "caret";
  const icon = document.createElement("span");
  icon.className = "icon";
  icon.innerHTML = fileIcon(kind);
  const label = document.createElement("span");
  label.className = "label";
  label.textContent = entry.name;
  row.append(caret, icon, label);

  row.onclick = () => selectRow(row, entry.path);
  if (kind === "env") {
    row.title = "Double-click to edit";
    row.ondblclick = () => void editEnv(parent.path);
  }
  row.oncontextmenu = (ev) => {
    ev.preventDefault();
    ev.stopPropagation();
    selectRow(row, entry.path);
    void showContextMenu(parent.path, parent.isModule);
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

  row.append(caret, icon, label);
  if (entry.isModule) {
    const badge = document.createElement("span");
    badge.className = "badge";
    badge.textContent = "tf";
    row.append(badge);
  }
  if (entry.hasEnvFile) {
    const badge = document.createElement("span");
    badge.className = "badge env";
    badge.textContent = "env";
    badge.title = ".gestalt.env in this folder applies here and to all subfolders";
    row.append(badge);
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
    void showContextMenu(entry.path, entry.isModule);
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
    // open_root also tells the backend where .gestalt.env lookup stops.
    const entry = await invoke<DirEntry>("open_root", { path: currentRoot });
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
  expanded.clear();
  expanded.add(path); // start with the root expanded
  selectedPath = null;
  await renderTree();
  void resetCommits();
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

/** Startup folder: command line / GESTALT_ROOT if set, else the last one opened. */
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
    last = localStorage.getItem(LAST_ROOT_KEY);
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
