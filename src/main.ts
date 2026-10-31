import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Menu, MenuItem, PredefinedMenuItem } from "@tauri-apps/api/menu";
import { ask, message, open } from "@tauri-apps/plugin-dialog";

// ---- Backend types ---------------------------------------------------------

type Action = "init" | "plan" | "apply";

interface DirEntry {
  name: string;
  path: string;
  hasChildren: boolean;
  isModule: boolean;
}

interface OutputEvent {
  runId: number;
  stream: "stdout" | "stderr";
  line: string;
}

interface ExitEvent {
  runId: number;
  code: number | null;
  cancelled: boolean;
  error: string | null;
}

// ---- Run state ---------------------------------------------------------------

type RunStatus = "running" | "succeeded" | "failed" | "cancelled";

interface Run {
  id: number;
  dir: string;
  action: Action;
  status: RunStatus;
  lines: { stream: string; line: string }[];
  exit?: ExitEvent;
}

const runs = new Map<number, Run>();
let selectedRunId: number | null = null;
// Output/exit events can arrive before `run_tofu` resolves with the id.
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

async function startRun(dir: string, action: Action) {
  if (action === "apply") {
    const ok = await ask(`Run "tofu apply -auto-approve" in\n${dir}?\n\nThis will change real infrastructure.`, {
      title: "Confirm apply",
      kind: "warning",
      okLabel: "Apply",
      cancelLabel: "Cancel",
    });
    if (!ok) return;
  }

  let id: number;
  try {
    id = await invoke<number>("run_tofu", { dir, action });
  } catch (e) {
    await message(String(e), { title: "Could not start tofu", kind: "error" });
    return;
  }

  const run: Run = { id, dir, action, status: "running", lines: [] };
  const pending = early.get(id);
  if (pending) {
    run.lines.push(...pending.lines);
    early.delete(id);
  }
  runs.set(id, run);
  if (pending?.exit) applyExit(run, pending.exit);
  selectRun(id);
}

function applyExit(run: Run, e: ExitEvent) {
  run.exit = e;
  run.status = e.cancelled ? "cancelled" : e.code === 0 ? "succeeded" : "failed";
  if (e.error) run.lines.push({ stream: "stderr", line: e.error });
}

// ---- Run rendering ---------------------------------------------------------

function renderTabs() {
  const tabs = $("run-tabs");
  tabs.replaceChildren(
    ...[...runs.values()].map((r) => {
      const b = document.createElement("button");
      b.className = `run-tab ${r.status}` + (r.id === selectedRunId ? " selected" : "");
      b.textContent = `${r.action} · ${relName(r.dir)}`;
      b.title = r.dir;
      b.onclick = () => selectRun(r.id);
      return b;
    }),
  );
}

function lineNode(l: Run["lines"][number]): Node {
  if (l.stream === "stderr") {
    const s = document.createElement("span");
    s.className = "stderr";
    s.textContent = l.line + "\n";
    return s;
  }
  return document.createTextNode(l.line + "\n");
}

function renderHeader() {
  const run = selectedRunId != null ? runs.get(selectedRunId) : undefined;
  $("run-header").hidden = !run;
  if (!run) return;
  $("run-title").textContent = `tofu ${run.action} — ${run.dir}`;
  const status = $("run-status");
  status.className = `status ${run.status}`;
  status.textContent =
    run.status === "running" ? "running…" : run.exit?.code != null ? `${run.status} (exit ${run.exit.code})` : run.status;
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
  applyExit(run, payload);
  if (run.id === selectedRunId) selectRun(run.id);
  else renderTabs();
});

$("run-cancel").onclick = () => {
  if (selectedRunId != null) invoke("cancel_tofu", { runId: selectedRunId }).catch(console.error);
};

$("run-close").onclick = () => {
  if (selectedRunId == null) return;
  runs.delete(selectedRunId);
  const remaining = [...runs.keys()];
  selectRun(remaining.length ? remaining[remaining.length - 1] : null);
};

// ---- Folder tree -------------------------------------------------------------

let currentRoot: string | null = null;

async function showContextMenu(dir: string, isModule: boolean) {
  const item = (action: Action, text: string) =>
    MenuItem.new({ text, enabled: isModule, action: () => void startRun(dir, action) });
  const menu = await Menu.new({
    items: [
      await item("plan", "tofu plan"),
      await item("apply", "tofu apply…"),
      await PredefinedMenuItem.new({ item: "Separator" }),
      await item("init", "tofu init"),
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

  const [kids, isModule] = await Promise.all([
    invoke<DirEntry[]>("list_dir", { path }),
    invoke<boolean>("is_module", { path }),
  ]);
  const root = makeNode({ name: baseName(path), path, hasChildren: kids.length > 0, isModule }, 0);
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

invoke<string>("tofu_version")
  .then((v) => ($("tofu-version").textContent = v))
  .catch((e) => {
    const el = $("tofu-version");
    el.textContent = "tofu not found";
    el.title = String(e);
    el.classList.add("error");
  });

try {
  const last = localStorage.getItem(LAST_ROOT_KEY);
  if (last) loadRoot(last).catch(() => ($("tree-hint").hidden = false));
} catch {
  /* storage unavailable */
}
