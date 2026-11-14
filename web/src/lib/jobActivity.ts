import type { RecentJob } from "../api";

const kindLabels: Record<RecentJob["kind"], string> = {
  scan: "Scan",
  rename: "Rename",
  rename_undo: "Undo Rename",
  remux: "Remux",
  convert_to_mkv: "Convert to MKV",
  mux_subtitles: "Mux Subtitles",
  extract_subtitles: "Extract Subtitles",
  property_edit: "Edit Tracks",
  library_audit: "Library Audit",
  cache_reconcile: "Cache Refresh",
  media_server_sync: "Media Server Sync"
};

// Where each kind of job is started, to run it again.
const kindPages: Partial<Record<RecentJob["kind"], { to: string; label: string }>> = {
  scan: { to: "/dashboard/scan", label: "Scan" },
  rename: { to: "/rename", label: "Rename Files" },
  rename_undo: { to: "/rename", label: "Rename Files" },
  remux: { to: "/tracks", label: "Tracks" },
  convert_to_mkv: { to: "/tracks/convert", label: "Tracks" },
  mux_subtitles: { to: "/subtitles", label: "Subtitles" },
  extract_subtitles: { to: "/subtitles", label: "Subtitles" },
  property_edit: { to: "/tracks/properties", label: "Tracks" }
};

export function jobKindLabel(job: RecentJob) {
  return kindLabels[job.kind] ?? job.kind;
}

export function jobPage(job: RecentJob) {
  return kindPages[job.kind] ?? null;
}

export function jobFinishedCount(job: RecentJob) {
  return job.completed + job.failed + job.skipped;
}

/** A running job in one line, the way the sidebar words the jobs this tab started. */
export function describeActiveJob(label: string, job: RecentJob) {
  if (job.status === "Queued" || job.status === "WaitingForResources") return `${label}: queued`;
  if (job.status === "Canceling") return `${label}: canceling`;
  if (!job.total) return `${label}: preparing`;
  const file = currentFileText(job);
  return `${label}: ${jobFinishedCount(job)}/${job.total}${file ? ` · ${file}` : ""}`;
}

/** The file a job is on, by name: jobs report it by full path. A scan reports no progress within a file. */
export function currentFileText(job: { kind?: string; currentFile: string; currentFilePercent: number }) {
  const name = job.currentFile.split(/[\\/]/).pop();
  if (!name) return "";
  return job.kind === "scan" ? name : `${name} ${job.currentFilePercent}%`;
}

const units: Array<[Intl.RelativeTimeFormatUnit, number]> = [["day", 86_400], ["hour", 3_600], ["minute", 60]];
const relative = new Intl.RelativeTimeFormat("en", { numeric: "auto" });

/** "5 minutes ago"; under a minute reads "just now". */
export function timeAgo(utc: string | null | undefined, now = Date.now()) {
  if (!utc) return "";
  const seconds = Math.round((new Date(utc).getTime() - now) / 1000);
  if (Number.isNaN(seconds)) return "";
  for (const [unit, size] of units) {
    if (Math.abs(seconds) >= size) return relative.format(Math.trunc(seconds / size), unit);
  }
  return "just now";
}

/** "42 s", "3 min 05 s", "1 h 12 min"; nothing for a job that took under a second. */
export function duration(startUtc: string | null | undefined, endUtc: string | null | undefined) {
  if (!startUtc || !endUtc) return "";
  const seconds = Math.max(0, Math.round((new Date(endUtc).getTime() - new Date(startUtc).getTime()) / 1000));
  if (Number.isNaN(seconds) || seconds === 0) return "";
  if (seconds < 60) return `${seconds} s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min ${String(seconds % 60).padStart(2, "0")} s`;
  return `${Math.floor(seconds / 3600)} h ${String(Math.floor((seconds % 3600) / 60)).padStart(2, "0")} min`;
}

/** A scan that finishes with unreadable files still did its job; those files are reported with the scan. */
export function jobFailed(job: RecentJob) {
  return job.status === "Failed" || (job.kind !== "scan" && job.status === "Completed" && job.failed > 0);
}

/** Failures of the last day that no later clean run of the same kind has settled. `jobs` is newest first. */
export function unresolvedFailures(jobs: RecentJob[], now = Date.now()) {
  return jobs.filter((job, index) => jobFailed(job)
    && now - new Date(job.completedUtc ?? job.createdUtc).getTime() < 86_400_000
    && !jobs.slice(0, index).some((newer) => newer.kind === job.kind && newer.status === "Completed" && newer.failed === 0));
}

const plural = (count: number, noun: string) => `${count} ${noun}${count === 1 ? "" : "s"}`;

/** What a job has done so far, or how it ended. */
export function jobResultText(job: RecentJob) {
  const finished = jobFinishedCount(job);
  switch (job.status) {
    case "Queued": return "Queued";
    case "WaitingForResources": return "Waiting for another job";
    case "Canceling": return "Canceling";
    case "Running": return job.total ? `${finished} of ${plural(job.total, "file")}` : "Preparing";
    case "Canceled": return `Canceled at ${finished} of ${job.total}`;
    case "Skipped": return "Nothing to do";
    case "Failed": return job.failed ? `${job.failed} of ${job.total} failed` : "Failed";
    default: {
      const parts = [job.kind === "scan" ? plural(job.total, "file") : `${job.completed} done`];
      if (job.failed) parts.push(job.kind === "scan" ? `${job.failed} unreadable` : `${job.failed} failed`);
      if (job.skipped) parts.push(`${job.skipped} skipped`);
      return parts.join(", ");
    }
  }
}

/** A batch error without its generic lead-in, so a row opens on the first file that failed. */
export function jobErrorText(job: RecentJob) {
  return (job.error ?? "").replace(/^internal error: (?=\d+ of \d+ files failed: )/, "");
}
