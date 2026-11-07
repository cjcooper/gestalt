// Side pane: resizable via the splitter, with Files and Commits tabs.

import { invoke } from "@tauri-apps/api/core";
import { icons } from "./icons";

const $ = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;

const store = {
  get(key: string): string | null {
    try {
      return localStorage.getItem(key);
    } catch {
      return null;
    }
  },
  set(key: string, value: string) {
    try {
      localStorage.setItem(key, value);
    } catch {
      /* storage unavailable */
    }
  },
};

// ---- Resizing ------------------------------------------------------------------

const WIDTH_KEY = "gestalt.sidebarWidth";
const DEFAULT_WIDTH = 320;
const MIN_WIDTH = 180;
const MIN_MAIN = 320; // keep the output pane usable

function clampWidth(w: number): number {
  return Math.round(Math.max(MIN_WIDTH, Math.min(w, window.innerWidth - MIN_MAIN)));
}

function setWidth(w: number, persist = true) {
  const width = clampWidth(w);
  $("sidebar").style.width = `${width}px`;
  $("splitter").setAttribute("aria-valuenow", String(width));
  if (persist) store.set(WIDTH_KEY, String(width));
}

function initSplitter() {
  const sidebar = $("sidebar");
  const splitter = $("splitter");
  setWidth(Number(store.get(WIDTH_KEY)) || DEFAULT_WIDTH, false);

  splitter.addEventListener("pointerdown", (ev) => {
    ev.preventDefault();
    splitter.setPointerCapture(ev.pointerId);
    document.body.classList.add("resizing");
    const left = sidebar.getBoundingClientRect().left;
    const move = (e: PointerEvent) => setWidth(e.clientX - left, false);
    const up = (e: PointerEvent) => {
      splitter.releasePointerCapture(e.pointerId);
      splitter.removeEventListener("pointermove", move);
      splitter.removeEventListener("pointerup", up);
      document.body.classList.remove("resizing");
      setWidth(sidebar.getBoundingClientRect().width);
    };
    splitter.addEventListener("pointermove", move);
    splitter.addEventListener("pointerup", up);
  });
  splitter.addEventListener("dblclick", () => setWidth(DEFAULT_WIDTH));
  splitter.addEventListener("keydown", (ev) => {
    const step = ev.shiftKey ? 50 : 10;
    const w = sidebar.getBoundingClientRect().width;
    if (ev.key === "ArrowLeft") setWidth(w - step);
    else if (ev.key === "ArrowRight") setWidth(w + step);
    else return;
    ev.preventDefault();
  });
  // Keep within bounds when the window shrinks (skip while hidden: it measures 0).
  window.addEventListener("resize", () => {
    const w = sidebar.getBoundingClientRect().width;
    if (w > 0) setWidth(w, false);
  });
}

// ---- Tabs ----------------------------------------------------------------------

type Tab = "files" | "commits";
/**
 * The tab the user wants: Commits by default for each opened folder, then
 * whatever they click. Kept in memory so refreshes don't move them; shown as
 * Files while the folder isn't a git repo.
 */
let wantedTab: Tab = "commits";

function showTab(want: Tab) {
  const tab = want === "commits" && !repoAvailable ? "files" : want;
  document.querySelectorAll<HTMLElement>("[data-tab]").forEach((b) => {
    const on = b.dataset.tab === tab;
    b.classList.toggle("active", on);
    b.setAttribute("aria-selected", String(on));
  });
  document.querySelectorAll<HTMLElement>("[data-panel]").forEach((p) => (p.hidden = p.dataset.panel !== tab));
  if (tab === "commits" && rootOpen && !commitsLoaded) void loadCommits(true);
}

// ---- Commits -------------------------------------------------------------------

interface Commit {
  hash: string;
  shortHash: string;
  parents: string[];
  isMerge: boolean;
  authorName: string;
  authorEmail: string;
  date: string;
  subject: string;
  refs: string[];
  /** Folders holding a main.tf that this commit changed (vs. its first parent). */
  modules: ModuleChange[];
}

interface ModuleChange {
  path: string;
  deleted: boolean;
  /** Folders earlier in the same list whose state this one reads. */
  dependsOn: string[];
}

interface GitLog {
  branch: string | null;
  /** Repo top-level folder on disk; module paths are relative to it. */
  repoRoot: string;
  commits: Commit[];
  hasMore: boolean;
  /** The server has changes for this branch that aren't local (as of the last fetch). */
  remoteHasChanges: boolean;
  /** Present on the first page only. */
  uncommitted: { fileCount: number; modules: ModuleChange[] } | null;
}

const PAGE = 200;
/** No git queries until a folder is open. */
let rootOpen = false;
/** Whether the open folder is in a git repo; the Commits tab is hidden if not. */
let repoAvailable = false;
let commitsLoaded = false;
let loadedCount = 0;
let commitsGeneration = 0;
let selectedCommit: string | null = null;
/**
 * Folder lists the user expanded (commit hash, or UNCOMMITTED), kept across
 * refreshes. Commits start collapsed so the Plan/Apply buttons don't crowd the
 * list; the uncommitted row starts expanded (it's what you're working on).
 */
const UNCOMMITTED = "uncommitted";
const expanded = new Set<string>([UNCOMMITTED]);

function relativeDate(iso: string): string {
  const then = new Date(iso).getTime();
  const s = Math.round((Date.now() - then) / 1000);
  if (s < 60) return "just now";
  const units: [number, string][] = [
    [60, "minute"],
    [3600, "hour"],
    [86400, "day"],
    [86400 * 7, "week"],
    [86400 * 30, "month"],
    [86400 * 365, "year"],
  ];
  let [size, name] = units[0];
  for (const u of units) if (s >= u[0]) [size, name] = u;
  const n = Math.floor(s / size);
  return `${n} ${name}${n === 1 ? "" : "s"} ago`;
}

function refBadge(ref: string): HTMLElement {
  const el = document.createElement("span");
  el.className = "ref";
  if (ref.startsWith("HEAD -> ")) {
    el.classList.add("head");
    el.textContent = ref.slice(8);
  } else if (ref === "HEAD") {
    el.classList.add("head");
    el.textContent = "HEAD";
  } else if (ref.startsWith("tag: ")) {
    el.classList.add("tag");
    el.textContent = ref.slice(5);
  } else {
    el.classList.add(ref.includes("/") ? "remote" : "local");
    el.textContent = ref;
  }
  return el;
}

/** Repo top level of the loaded list, for turning module paths into local folders. */
let repoRoot: string | null = null;
type FolderAction = (dir: string) => void;
// Set by initSidebar, which runs before any folder list can be rendered.
let onPlan!: FolderAction;
let onApply!: FolderAction;

function localFolder(path: string): string | null {
  if (!repoRoot) return null;
  return path === "." ? repoRoot : `${repoRoot.replace(/\/$/, "")}/${path}`;
}

/**
 * Folders holding a main.tf, one full path per line, in run order (the
 * backend puts folders whose state others read first); deleted folders last.
 */
function moduleList(modules: ModuleChange[]): HTMLElement {
  const mods = document.createElement("ol");
  mods.className = "module-changes";
  let step = 0;
  for (const m of modules) {
    const item = document.createElement("li");
    item.className = m.deleted ? "deleted" : "";
    item.title = m.deleted
      ? `${m.path} — folder deleted`
      : m.dependsOn.length
        ? `${m.path}\nRun after: ${m.dependsOn.join(", ")} (reads their state)`
        : m.path;
    const num = document.createElement("span");
    num.className = "step" + (m.dependsOn.length ? " has-deps" : "");
    num.textContent = m.deleted ? "–" : String(++step);
    item.append(num);
    const path = document.createElement("span");
    path.className = "path";
    path.textContent = m.path;
    item.append(path);
    if (m.deleted) {
      const tag = document.createElement("span");
      tag.className = "deleted-tag";
      tag.textContent = "deleted";
      item.append(tag);
    } else {
      const dir = localFolder(m.path);
      if (dir) {
        const button = (label: string, icon: string, cls: string, title: string, action: FolderAction) => {
          const b = document.createElement("button");
          b.className = `folder-btn ${cls}`;
          b.innerHTML = icon;
          b.append(label);
          b.title = title;
          b.onclick = (ev) => {
            ev.stopPropagation(); // don't also select the commit row
            action(dir);
          };
          return b;
        };
        const actions = document.createElement("span");
        actions.className = "folder-actions";
        actions.append(
          button("Plan", icons.play, "plan-btn", `tofu plan in ${dir}`, onPlan),
          button("Apply", icons.check, "apply-btn", `Plan, review, then apply in ${dir}`, onApply),
        );
        item.append(actions);
      }
    }
    mods.append(item);
  }
  return mods;
}

/**
 * A "▸ N folders" toggle plus the folder list, collapsed unless `key` is in
 * `expanded`.
 */
function foldersSection(key: string, modules: ModuleChange[]): HTMLElement[] {
  const list = moduleList(modules);
  const toggle = document.createElement("button");
  toggle.className = "folders-toggle";
  const caret = document.createElement("span");
  caret.className = "chevron";
  caret.innerHTML = icons.chevronRight;
  const label = document.createElement("span");
  const n = modules.length;
  const plural = n === 1 ? "" : "s";
  label.textContent = modules.some((m) => !m.deleted)
    ? `${n} folder${plural}`
    : `${n} deleted folder${plural} · nothing to run`;
  toggle.append(caret, label);
  const render = () => {
    const open = expanded.has(key);
    list.hidden = !open;
    toggle.classList.toggle("open", open);
    toggle.setAttribute("aria-expanded", String(open));
    toggle.title = open ? "Hide folders" : "Show folders and their Plan / Apply buttons";
  };
  toggle.onclick = (ev) => {
    ev.stopPropagation(); // don't also select the commit row
    if (expanded.has(key)) expanded.delete(key);
    else expanded.add(key);
    render();
  };
  render();
  return [toggle, list];
}

/** Shown instead of the folders toggle when a commit changed no OpenTofu folder. */
function nothingToRun(): HTMLElement {
  const note = document.createElement("div");
  note.className = "nothing-to-run";
  note.textContent = "Nothing to run · no OpenTofu folders changed";
  note.title = "This commit didn't change any folder holding a main.tf, so there's nothing to plan or apply.";
  return note;
}

/** Pinned first row: module folders with uncommitted changes. */
function uncommittedRow(u: NonNullable<GitLog["uncommitted"]>): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "commit uncommitted" + (u.fileCount ? " dirty" : "");

  const icon = document.createElement("span");
  icon.className = "icon";
  icon.innerHTML = icons.pencil;

  const subject = document.createElement("div");
  subject.className = "subject";
  subject.textContent = "Uncommitted changes";

  const meta = document.createElement("div");
  meta.className = "meta";
  meta.textContent =
    u.fileCount === 0
      ? "Working tree clean"
      : `${u.fileCount} file${u.fileCount === 1 ? "" : "s"} changed`;

  const body = document.createElement("div");
  body.className = "commit-body";
  body.append(subject, meta);
  if (u.modules.length) body.append(...foldersSection(UNCOMMITTED, u.modules));
  else if (u.fileCount) body.append(nothingToRun());

  li.title = "Staged, unstaged and untracked changes compared with HEAD (ignored files excluded)";
  li.append(icon, body);
  return li;
}

function commitRow(c: Commit): HTMLLIElement {
  const li = document.createElement("li");
  li.className = "commit" + (c.isMerge ? " merge" : "") + (c.hash === selectedCommit ? " selected" : "");
  li.title = `${c.hash}\n${c.authorName} <${c.authorEmail}>\n${new Date(c.date).toLocaleString()}` +
    (c.isMerge ? `\nMerge of ${c.parents.length} parents` : "");

  const icon = document.createElement("span");
  icon.className = "icon";
  icon.innerHTML = c.isMerge ? icons.gitMerge : icons.gitCommit;

  const subject = document.createElement("div");
  subject.className = "subject";
  subject.textContent = c.subject;

  const meta = document.createElement("div");
  meta.className = "meta";
  const hash = document.createElement("code");
  hash.textContent = c.shortHash;
  meta.append(hash, ` · ${c.authorName} · ${relativeDate(c.date)}`);

  const body = document.createElement("div");
  body.className = "commit-body";
  body.append(subject, meta);
  if (c.refs.length) {
    const refs = document.createElement("div");
    refs.className = "refs";
    refs.append(...c.refs.map(refBadge));
    body.append(refs);
  }

  body.append(...(c.modules.length ? foldersSection(c.hash, c.modules) : [nothingToRun()]));

  li.append(icon, body);
  li.onclick = () => {
    document.querySelectorAll(".commit.selected").forEach((el) => el.classList.remove("selected"));
    li.classList.add("selected");
    selectedCommit = c.hash;
  };
  return li;
}

async function loadCommits(reset: boolean) {
  const gen = reset ? ++commitsGeneration : commitsGeneration;
  const list = $("commit-list");
  const status = $("commits-status");
  const more = $<HTMLButtonElement>("commits-more");
  more.disabled = true;
  if (reset) {
    loadedCount = 0;
    status.textContent = "Loading…";
    status.className = "commits-status muted";
    status.hidden = false;
  }
  try {
    // On refresh, reload everything that was showing so the list doesn't shrink.
    const limit = reset ? Math.max(PAGE, list.querySelectorAll(".commit:not(.uncommitted)").length) : PAGE;
    const log = await invoke<GitLog>("git_log", { skip: loadedCount, limit });
    if (gen !== commitsGeneration) return;
    commitsLoaded = true;
    repoRoot = log.repoRoot;
    const header: (Node | string)[] = [
      Object.assign(document.createElement("span"), { className: "icon", innerHTML: icons.gitBranch }),
      log.branch ?? "detached HEAD",
    ];
    if (log.remoteHasChanges) {
      const badge = document.createElement("span");
      badge.className = "remote-changes";
      badge.innerHTML = icons.cloudDown;
      badge.append("Server has changes");
      badge.title =
        "The server has changes for this branch that aren't in your local copy (as of the last fetch). " +
        "They aren't shown here; pull them with your usual git tools.";
      header.push(badge);
    }
    $("commits-branch").replaceChildren(...header);
    const rows = log.commits.map(commitRow);
    if (log.uncommitted) rows.unshift(uncommittedRow(log.uncommitted));
    if (reset) list.replaceChildren(...rows);
    else list.append(...rows);
    loadedCount += log.commits.length;
    status.hidden = loadedCount > 0;
    if (loadedCount === 0) {
      status.textContent = "No commits yet.";
      status.className = "commits-status muted";
    }
    more.hidden = !log.hasMore;
  } catch (e) {
    if (gen !== commitsGeneration) return;
    commitsLoaded = true; // don't retry on every tab switch; Refresh retries
    list.replaceChildren();
    $("commits-branch").textContent = "";
    status.textContent = String(e);
    status.className = "commits-status error";
    status.hidden = false;
    more.hidden = true;
  } finally {
    more.disabled = false;
  }
}

/** Reload commits now if the tab is showing, else on next view. */
export async function refreshCommits() {
  if (!rootOpen) return;
  commitsLoaded = false;
  // Re-checks repo status (it may have changed); showTab then reloads the list if visible.
  await updateRepoAvailability();
}

function commitsVisible(): boolean {
  return !document.querySelector<HTMLElement>('[data-panel="commits"]')!.hidden;
}

let uncommittedGeneration = 0;

/** Files changed on disk: update just the pinned "Uncommitted changes" row. */
export async function refreshUncommitted() {
  if (!rootOpen || !repoAvailable) return;
  if (!commitsVisible() || !commitsLoaded) {
    commitsLoaded = false; // reload fully when the tab is next shown
    return;
  }
  const gen = ++uncommittedGeneration;
  const listGen = commitsGeneration;
  try {
    const u = await invoke<NonNullable<GitLog["uncommitted"]>>("git_uncommitted");
    // Skip if superseded, or a full reload started meanwhile (it includes this row).
    if (gen !== uncommittedGeneration || listGen !== commitsGeneration) return;
    const row = uncommittedRow(u);
    const list = $("commit-list");
    const old = list.querySelector(".commit.uncommitted");
    if (old) old.replaceWith(row);
    else list.prepend(row);
  } catch (e) {
    console.error(e);
  }
}

/** Show the Commits tab only for git repos; falls back to Files otherwise. */
async function updateRepoAvailability() {
  repoAvailable = await invoke<boolean>("git_is_repo").catch(() => false);
  document.querySelector<HTMLElement>('[data-tab="commits"]')!.hidden = !repoAvailable;
  showTab(wantedTab);
}

/** A different folder was opened: forget the previous repo's list. */
export async function resetCommits() {
  rootOpen = true;
  wantedTab = "commits"; // each newly opened folder starts on Commits
  commitsGeneration++;
  commitsLoaded = false;
  selectedCommit = null;
  expanded.clear();
  expanded.add(UNCOMMITTED);
  $("commit-list").replaceChildren();
  await updateRepoAvailability();
}

/**
 * `plan` / `apply` run those procedures for a local folder from a commit's
 * folder list (apply = plan, confirm, apply that saved plan, in one pane).
 */
export function initSidebar(plan: FolderAction, apply: FolderAction) {
  onPlan = plan;
  onApply = apply;
  initSplitter();
  document.querySelectorAll<HTMLElement>("[data-tab]").forEach((b) => {
    b.onclick = () => {
      wantedTab = b.dataset.tab as Tab;
      showTab(wantedTab);
    };
  });
  $("commits-more").onclick = () => void loadCommits(false);
  // Commits stays hidden until a folder is open and known to be a repo.
  document.querySelector<HTMLElement>('[data-tab="commits"]')!.hidden = true;
  showTab("files");
}
