import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { ask, message, open } from "@tauri-apps/plugin-dialog";

// ---- Backend types ---------------------------------------------------------

type RunKind = "init" | "plan" | "apply";

interface DirEntry {
  name: string;
  path: string;
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
  /** Plan run started from "tofu apply…": prompt to apply when it finishes. */
  applyWhenReady?: boolean;
  /** Apply runs: the plan run they applied. */
  fromPlan?: number;
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

async function startRun(dir: string, kind: "init" | "plan", opts: { applyWhenReady?: boolean } = {}) {
  let id: number;
  try {
    id = await invoke<number>("run_tofu", { dir, action: kind });
  } catch (e) {
    await message(String(e), { title: "Could not start tofu", kind: "error" });
    return;
  }
  registerRun({ id, dir, kind, status: "running", lines: [], planState: "none", ...opts });
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

  let id: number;
  try {
    id = await invoke<number>("apply_plan", { planRunId: plan.id });
  } catch (e) {
    await message(String(e), { title: "Could not apply plan", kind: "error" });
    return;
  }
  plan.planState = "applied";
  registerRun({ id, dir: plan.dir, kind: "apply", status: "running", lines: [], planState: "none", fromPlan: plan.id });
}

function handleExit(run: Run, e: ExitEvent) {
  run.exit = e;
  run.status = e.cancelled ? "cancelled" : e.success ? "succeeded" : "failed";
  if (e.error) run.lines.push({ stream: "stderr", line: e.error });
  if (run.kind === "plan" && e.hasChanges) run.planState = "ready";

  if (run.id === selectedRunId) selectRun(run.id);
  else renderTabs();

  if (run.applyWhenReady && run.planState === "ready") {
    selectRun(run.id);
    void confirmAndApply(run);
  }
}

// ---- Run rendering ---------------------------------------------------------

function statusText(run: Run): string {
  if (run.status === "running") return "running…";
  if (run.kind === "plan" && run.status === "succeeded") {
    if (!run.exit?.hasChanges) return "no changes";
    const s = planSummary(run) ?? "changes pending";
    return run.planState === "applied" ? `${s} (applied)` : run.planState === "ready" ? s : `${s} (discarded)`;
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
  const from = run.fromPlan != null ? ` (from plan #${run.fromPlan})` : "";
  $("run-title").textContent = `tofu ${run.kind}${from} — ${run.dir}`;
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

async function showContextMenu(dir: string, isModule: boolean) {
  const item = (text: string, action: () => void) => MenuItem.new({ text, enabled: isModule, action });
  const menu = await Menu.new({
    items: [
      await item("tofu plan", () => void startRun(dir, "plan")),
      // Plans first, then asks to apply that exact saved plan.
      await item("tofu apply…", () => void startRun(dir, "plan", { applyWhenReady: true })),
      await PredefinedMenuItem.new({ item: "Separator" }),
      await item("tofu init", () => void startRun(dir, "init")),
    ],
  });
  await menu.popup();
}

function makeNode(entry: DirEntry, depth: number): HTMLLIElement {
  const li = document.createElement("li");
  li.setAttribute("role", "treeitem");

  const row = document.createElement("div");
  row.className = "tree-row" + (entry.isModule ? " module" : "");
  row.style.paddingLeft = `${depth * 14 + 6}px`;
  row.title = entry.path;

  const caret = document.createElement("span");
  caret.className = "caret";
  caret.textContent = entry.hasChildren ? "▸" : "";

  const label = document.createElement("span");
  label.className = "label";
  label.textContent = entry.name;

  row.append(caret, label);
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

  const toggle = async () => {
    if (!entry.hasChildren) return;
    if (!loaded) {
      loaded = true;
      try {
        const kids = await invoke<DirEntry[]>("list_dir", { path: entry.path });
        children.replaceChildren(...kids.map((k) => makeNode(k, depth + 1)));
      } catch (e) {
        loaded = false;
        console.error(e);
        return;
      }
    }
    children.hidden = !children.hidden;
    caret.textContent = children.hidden ? "▸" : "▾";
  };

  row.onclick = () => {
    document.querySelectorAll(".tree-row.selected").forEach((el) => el.classList.remove("selected"));
    row.classList.add("selected");
    void toggle();
  };
  row.oncontextmenu = (ev) => {
    ev.preventDefault();
    ev.stopPropagation();
    document.querySelectorAll(".tree-row.selected").forEach((el) => el.classList.remove("selected"));
    row.classList.add("selected");
    void showContextMenu(entry.path, entry.isModule);
  };

  li.append(row, children);
  return li;
}

async function loadRoot(path: string) {
  currentRoot = path;
  try {
    localStorage.setItem(LAST_ROOT_KEY, path);
  } catch {
    /* storage unavailable */
  }
  $("root-path").textContent = path;
  $("tree-hint").hidden = true;

  // open_root also tells the backend where .gestalt.env lookup stops.
  const entry = await invoke<DirEntry>("open_root", { path });
  const root = makeNode(entry, 0);
  $("tree").replaceChildren(root);
  // Expand the root immediately.
  (root.querySelector(".tree-row") as HTMLElement).click();
  renderTabs();
}

$("open-root").onclick = async () => {
  const picked = await open({ directory: true, multiple: false, defaultPath: currentRoot ?? undefined });
  if (typeof picked === "string") await loadRoot(picked);
};

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

try {
  const last = localStorage.getItem(LAST_ROOT_KEY);
  if (last) loadRoot(last).catch(() => ($("tree-hint").hidden = false));
} catch {
  /* storage unavailable */
}
