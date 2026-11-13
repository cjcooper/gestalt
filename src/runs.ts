// Run model shared by the UI, and the pure helpers that describe a run.

import { stripAnsi, type AnsiState } from "./ansi";

// ---- Backend types ---------------------------------------------------------

export type RunKind = "plan" | "apply" | "import" | "clean";

export interface DirEntry {
  name: string;
  path: string;
  isDir: boolean;
  hasChildren: boolean;
  isModule: boolean;
  /** A symlink: edits change the real file, wherever else it's linked from. */
  isSymlink: boolean;
}

export interface OutputEvent {
  runId: number;
  /** "gestalt" lines are the app's own notes, e.g. which cloud settings applied. */
  stream: "stdout" | "stderr" | "gestalt";
  line: string;
}

export interface ExitEvent {
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

export type RunStatus = "running" | "succeeded" | "failed" | "cancelled";

/** Lifecycle of a plan run's saved plan file. */
export type PlanState = "none" | "ready" | "applied" | "discarded";

export interface Run {
  id: number;
  dir: string;
  kind: RunKind;
  status: RunStatus;
  lines: { stream: string; line: string }[];
  exit?: ExitEvent;
  planState: PlanState;
  /** Started from "Apply…": prompt to apply once the plan finishes. */
  applyWhenReady?: boolean;
  /** Clean runs: what was freed, shown as the status. */
  summary?: string;
  /** Colour state each stream left set, while the run is shown. */
  ansi: Map<string, AnsiState>;
}

/** The "Plan: 1 to add, 0 to change, 0 to destroy." line, if tofu printed one. */
export function planSummary(run: Run): string | undefined {
  for (let i = run.lines.length - 1; i >= 0; i--) {
    const l = stripAnsi(run.lines[i].line).trim();
    if (/^Plan: \d+ to /.test(l)) return l;
  }
  return undefined;
}

export interface CleanSummary {
  paths: string[];
  dirs: number;
  files: number;
  bytes: number;
  errors: string[];
}

export function formatBytes(n: number): string {
  const units = ["bytes", "KB", "MB", "GB", "TB"];
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return i === 0 ? `${n} ${units[0]}` : `${n.toFixed(1)} ${units[i]}`;
}

export function describeClean(s: CleanSummary): string {
  const parts = [];
  if (s.dirs) parts.push(`${s.dirs} .terraform folder${s.dirs === 1 ? "" : "s"}`);
  if (s.files) parts.push(`${s.files} .terraform.lock.hcl file${s.files === 1 ? "" : "s"}`);
  return `${parts.join(" and ")} (${formatBytes(s.bytes)})`;
}

// ---- Run status ----------------------------------------------------------------

/** The short status shown in a run's header. */
export function statusText(run: Run): string {
  if (run.kind === "clean") {
    if (run.status === "running") return "cleaning…";
    return run.status === "failed" && run.summary ? `${run.summary}, with errors` : (run.summary ?? run.status);
  }
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
