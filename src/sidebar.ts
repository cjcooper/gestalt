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
const TAB_KEY = "gestalt.sidebarTab";
let activeTab: Tab = "files";

function showTab(tab: Tab) {
  activeTab = tab;
  store.set(TAB_KEY, tab);
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
}

interface GitLog {
  branch: string | null;
  commits: Commit[];
  hasMore: boolean;
}

const PAGE = 200;
/** No git queries until a folder is open. */
let rootOpen = false;
let commitsLoaded = false;
let loadedCount = 0;
let commitsGeneration = 0;
let selectedCommit: string | null = null;

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
    const limit = reset ? Math.max(PAGE, list.childElementCount) : PAGE;
    const log = await invoke<GitLog>("git_log", { skip: loadedCount, limit });
    if (gen !== commitsGeneration) return;
    commitsLoaded = true;
    $("commits-branch").replaceChildren(
      Object.assign(document.createElement("span"), { className: "icon", innerHTML: icons.gitBranch }),
      log.branch ?? "detached HEAD",
    );
    const rows = log.commits.map(commitRow);
    if (reset) list.replaceChildren(...rows);
    else list.append(...rows);
    loadedCount += log.commits.length;
    status.hidden = loadedCount > 0;
    if (loadedCount === 0) status.textContent = "No commits yet.";
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
export function refreshCommits() {
  if (!rootOpen) return;
  if (activeTab === "commits") void loadCommits(true);
  else commitsLoaded = false;
}

/** A different folder was opened: forget the previous repo's list. */
export function resetCommits() {
  rootOpen = true;
  commitsGeneration++;
  commitsLoaded = false;
  selectedCommit = null;
  $("commit-list").replaceChildren();
  refreshCommits();
}

export function initSidebar() {
  initSplitter();
  document.querySelectorAll<HTMLElement>("[data-tab]").forEach((b) => {
    b.onclick = () => showTab(b.dataset.tab as Tab);
  });
  $("commits-more").onclick = () => void loadCommits(false);
  const saved = store.get(TAB_KEY);
  showTab(saved === "commits" ? "commits" : "files");
}
