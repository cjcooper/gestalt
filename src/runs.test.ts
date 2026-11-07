import { describe, expect, it } from "vitest";
import { describeClean, formatBytes, planSummary, statusText, type ExitEvent, type Run } from "./runs";

const PLAN_LINE = "Plan: 1 to add, 0 to change, 0 to destroy.";

const exit = (extra: Partial<ExitEvent> = {}): ExitEvent => ({
  runId: 1,
  code: 0,
  success: true,
  cancelled: false,
  hasChanges: null,
  error: null,
  started: true,
  ...extra,
});

const run = (extra: Partial<Run> = {}): Run => ({
  id: 1,
  dir: "/repo/app",
  kind: "plan",
  status: "succeeded",
  lines: [],
  planState: "none",
  ansi: new Map(),
  ...extra,
});

const lines = (...ls: string[]) => ls.map((line) => ({ stream: "stdout", line }));

describe("planSummary", () => {
  it("finds the last Plan: line, ignoring colour codes and indentation", () => {
    expect(
      planSummary(run({ lines: lines("Plan: 9 to add, 0 to change, 0 to destroy.", `  \x1b[1m${PLAN_LINE}\x1b[0m  `, "done") })),
    ).toBe(PLAN_LINE);
  });

  it("is undefined without one", () => {
    expect(planSummary(run({ lines: lines("No changes.", "Plan: none") }))).toBeUndefined();
    expect(planSummary(run())).toBeUndefined();
  });
});

describe("formatBytes", () => {
  it.each([
    [0, "0 bytes"],
    [1023, "1023 bytes"],
    [1024, "1.0 KB"],
    [1536, "1.5 KB"],
    [5 * 1024 ** 2, "5.0 MB"],
    [2.25 * 1024 ** 3, "2.3 GB"],
    [1024 ** 4, "1.0 TB"],
    [2048 * 1024 ** 4, "2048.0 TB"],
  ])("%d → %s", (n, s) => {
    expect(formatBytes(n)).toBe(s);
  });
});

describe("describeClean", () => {
  const summary = (dirs: number, files: number, bytes = 2048) => ({ paths: [], dirs, files, bytes, errors: [] });

  it("names folders and lock files with plurals", () => {
    expect(describeClean(summary(1, 0))).toBe("1 .terraform folder (2.0 KB)");
    expect(describeClean(summary(3, 0))).toBe("3 .terraform folders (2.0 KB)");
    expect(describeClean(summary(0, 1, 10))).toBe("1 .terraform.lock.hcl file (10 bytes)");
    expect(describeClean(summary(2, 5))).toBe("2 .terraform folders and 5 .terraform.lock.hcl files (2.0 KB)");
  });
});

describe("statusText", () => {
  it("describes clean runs", () => {
    expect(statusText(run({ kind: "clean", status: "running" }))).toBe("cleaning…");
    expect(statusText(run({ kind: "clean", summary: "freed 1.0 KB" }))).toBe("freed 1.0 KB");
    expect(statusText(run({ kind: "clean", status: "failed", summary: "freed 1.0 KB" }))).toBe("freed 1.0 KB, with errors");
    expect(statusText(run({ kind: "clean", status: "failed" }))).toBe("failed");
  });

  it("describes running plans and applies", () => {
    expect(statusText(run({ status: "running" }))).toBe("running…");
    expect(statusText(run({ status: "running", planState: "applied" }))).toBe("applying…");
  });

  it("describes a finished apply", () => {
    expect(statusText(run({ planState: "applied", lines: lines(PLAN_LINE) }))).toBe(`applied — ${PLAN_LINE}`);
    expect(statusText(run({ planState: "applied" }))).toBe("applied");
  });

  it("describes a plan's outcome", () => {
    expect(statusText(run({ exit: exit({ hasChanges: false }) }))).toBe("no changes");
    const ready = { exit: exit({ hasChanges: true }), planState: "ready" as const };
    expect(statusText(run({ ...ready, lines: lines(PLAN_LINE) }))).toBe(PLAN_LINE);
    expect(statusText(run(ready))).toBe("changes pending");
    expect(statusText(run({ ...ready, kind: "apply" }))).toBe("changes pending (not applied)");
    expect(statusText(run({ ...ready, planState: "discarded", lines: lines(PLAN_LINE) }))).toBe(`${PLAN_LINE} (discarded)`);
  });

  it("falls back to the run status, with an exit code for failures", () => {
    expect(statusText(run({ status: "failed", exit: exit({ code: 1, success: false }) }))).toBe("failed (exit 1)");
    expect(statusText(run({ status: "failed", exit: exit({ code: null, success: false }) }))).toBe("failed");
    expect(statusText(run({ status: "failed" }))).toBe("failed");
    expect(statusText(run({ status: "cancelled", exit: exit({ code: 130, hasChanges: true }) }))).toBe("cancelled");
    expect(statusText(run({ kind: "import" }))).toBe("succeeded");
  });
});
