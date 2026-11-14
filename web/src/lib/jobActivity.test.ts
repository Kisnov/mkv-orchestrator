import { expect, it } from "vitest";

import type { RecentJob } from "../api";
import { describeActiveJob, duration, jobErrorText, jobPage, jobResultText, timeAgo, unresolvedFailures } from "./jobActivity";

const now = new Date("2026-11-14T12:00:00Z").getTime();

function job(patch: Partial<RecentJob>): RecentJob {
  return { id: "job", kind: "remux", status: "Completed", createdUtc: "2026-11-14T11:00:00Z", completedUtc: "2026-11-14T11:01:00Z", completed: 4, failed: 0, skipped: 0, total: 4, currentFile: "", currentFilePercent: 0, summary: "", ...patch };
}

it("words a running job the way the sidebar words a tracked one", () => {
  expect(describeActiveJob("Scan", job({ status: "Running", completed: 12, total: 128 }))).toBe("Scan: 12/128");
  expect(describeActiveJob("Scan", job({ status: "Running", completed: 0, total: 0 }))).toBe("Scan: preparing");
  expect(describeActiveJob("Remux", job({ status: "Running", completed: 1, total: 4, currentFile: "\\\\?\\C:\\media\\Show\\02.mkv", currentFilePercent: 40 }))).toBe("Remux: 1/4 · 02.mkv 40%");
  expect(describeActiveJob("Scan", job({ kind: "scan", status: "Running", completed: 1, total: 4, currentFile: "/media/Show/02.mkv", currentFilePercent: 100 }))).toBe("Scan: 1/4 · 02.mkv");
  expect(describeActiveJob("Remux", job({ status: "WaitingForResources" }))).toBe("Remux: queued");
});

it("says how a job ended", () => {
  expect(jobResultText(job({ kind: "scan", total: 128, completed: 126, failed: 2 }))).toBe("128 files, 2 unreadable");
  expect(jobResultText(job({ completed: 3, skipped: 1 }))).toBe("3 done, 1 skipped");
  expect(jobResultText(job({ status: "Failed", completed: 2, failed: 2 }))).toBe("2 of 4 failed");
  expect(jobResultText(job({ status: "Failed", completed: 0, total: 0 }))).toBe("Failed");
  expect(jobResultText(job({ status: "Canceled", completed: 1 }))).toBe("Canceled at 1 of 4");
  expect(jobResultText(job({ status: "Running", completed: 1, total: 1 }))).toBe("1 of 1 file");
});

it("sends a retry to the page the job came from, or nowhere", () => {
  const labels = new Map([["reorder", "Reorder Tracks"], ["old", "MKV Operations"]]);
  expect(jobPage(job({ id: "reorder" }), labels)).toEqual({ to: "/tracks/reorder", label: "Reorder Tracks" });
  // A remux can be any of four pages: without its page's name there is no telling.
  expect(jobPage(job({ id: "unknown" }), labels)).toBeNull();
  expect(jobPage(job({ id: "old" }), labels)).toBeNull();
  expect(jobPage(job({ kind: "property_edit" }), labels)).toEqual({ to: "/tracks/properties", label: "Edit Tracks" });
  expect(jobPage(job({ kind: "extract_subtitles" }), labels)).toEqual({ to: "/subtitles", label: "Subtitles" });
  expect(jobPage(job({ kind: "convert_to_mkv" }), labels)).toEqual({ to: "/tracks/convert", label: "Convert / Remux" });
  expect(jobPage(job({ kind: "scan" }), labels)).toEqual({ to: "/dashboard/scan", label: "Scan" });
});

it("opens a batch error on what failed", () => {
  expect(jobErrorText(job({ error: "internal error: 3 of 24 files failed: 04.mkv: tool failed" }))).toBe("3 of 24 files failed: 04.mkv: tool failed");
  expect(jobErrorText(job({ error: "internal error: disk full" }))).toBe("internal error: disk full");
  expect(jobErrorText(job({}))).toBe("");
});

it("counts a failure until a later clean run of the same page, or of the same kind when no page is known", () => {
  const none = new Map<string, string>();
  const failed = job({ id: "failed", status: "Failed", failed: 1 });
  expect(unresolvedFailures([failed], none, now).map((item) => item.id)).toEqual(["failed"]);
  expect(unresolvedFailures([job({ id: "retry" }), failed], none, now)).toEqual([]);
  // Another kind's success, or an older one, settles nothing.
  expect(unresolvedFailures([job({ id: "scan", kind: "scan" }), failed, job({ id: "older" })], none, now).map((item) => item.id)).toEqual(["failed"]);
  expect(unresolvedFailures([job({ id: "old", status: "Failed", completedUtc: "2026-11-12T11:00:00Z" })], none, now)).toEqual([]);
  // A clean Remove does not settle a failed Reorder, though both are remuxes.
  const pages = new Map([["failed", "Reorder Tracks"], ["remove", "Remove Tracks"], ["reorder", "Reorder Tracks"]]);
  expect(unresolvedFailures([job({ id: "remove" }), failed], pages, now).map((item) => item.id)).toEqual(["failed"]);
  expect(unresolvedFailures([job({ id: "reorder" }), failed], pages, now)).toEqual([]);
});

it("settles a failed scan with a later scan that finished, unreadable files or not", () => {
  const none = new Map<string, string>();
  const failedScan = job({ id: "failed", kind: "scan", status: "Failed" });
  // Unreadable files do not make a scan a failed job.
  expect(unresolvedFailures([job({ kind: "scan", failed: 2 })], none, now)).toEqual([]);
  expect(unresolvedFailures([job({ id: "rescan", kind: "scan", failed: 1 }), failedScan], none, now)).toEqual([]);
  expect(unresolvedFailures([job({ id: "again", kind: "scan", status: "Failed" }), failedScan], none, now).map((item) => item.id)).toEqual(["again", "failed"]);
});

it("formats ages and durations", () => {
  expect(timeAgo("2026-11-14T11:59:40Z", now)).toBe("just now");
  expect(timeAgo("2026-11-14T11:55:00Z", now)).toBe("5 minutes ago");
  expect(timeAgo("2026-11-13T11:00:00Z", now)).toBe("yesterday");
  expect(timeAgo(null, now)).toBe("");
  expect(duration("2026-11-14T11:00:00Z", "2026-11-14T11:00:42Z")).toBe("42 s");
  expect(duration("2026-11-14T11:00:00Z", "2026-11-14T11:00:00.300Z")).toBe("");
  expect(duration("2026-11-14T11:00:00Z", "2026-11-14T11:03:05Z")).toBe("3 min 05 s");
  expect(duration("2026-11-14T11:00:00Z", "2026-11-14T12:12:00Z")).toBe("1 h 12 min");
});
