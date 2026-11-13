import { Copy } from "lucide-react";

import type { OperationJobResponse } from "../api";
import { Dialog } from "./Dialog";

/** The failed files a batch error names, one per entry; a single-file job's error is its own entry. */
function failedFiles(job: OperationJobResponse): string[] {
  if (!job.error) return [];
  const batch = /^\d+ of \d+ files failed: ([\s\S]*)$/.exec(job.error);
  return batch ? batch[1].split("; ") : [job.error];
}

export function JobLogModal({ title, job, onClose }: { title: string; job: OperationJobResponse; onClose: () => void }) {
  const failed = failedFiles(job);
  const counts = `${job.completed} done, ${job.failed} failed, ${job.skipped} skipped of ${job.total}`;
  return (
    <Dialog title={title} onClose={onClose} className="w-[min(60rem,calc(100vw-3rem))]">
      <div className="min-h-0 flex-1 overflow-y-auto p-5">
        <div className="flex items-center justify-between gap-3 rounded-md border border-border bg-input px-4 py-3">
          <div className="min-w-0">
            <div className={["text-sm font-semibold", job.status === "Completed" ? "text-success" : "text-warning"].join(" ")}>{job.status}</div>
            <div className="mt-1 text-xs text-muted">{counts}</div>
          </div>
          <button type="button" onClick={() => void navigator.clipboard.writeText([`${job.status}: ${counts}`, ...failed, ...job.lines].join("\n"))} className="inline-flex h-9 shrink-0 items-center gap-2 rounded-md border border-border bg-button px-3 text-sm font-semibold text-muted hover:bg-button-hover hover:text-text">
            <Copy size={15} /> Copy log
          </button>
        </div>
        {failed.length ? (
          <section className="mt-5">
            <h3 className="mb-2 text-sm font-semibold text-text">Failed files <span className="text-muted">({job.failed})</span></h3>
            <ul className="overflow-hidden rounded-md border border-warning bg-input">
              {failed.map((entry, index) => <li key={index} className="break-words border-b border-border px-4 py-2 text-xs leading-5 text-text [overflow-wrap:anywhere] last:border-0">{entry}</li>)}
            </ul>
          </section>
        ) : null}
        <section className="mt-5">
          <h3 className="mb-2 text-sm font-semibold text-text">Job log <span className="text-muted">({job.lines.length})</span></h3>
          {job.lines.length
            ? <pre className="whitespace-pre-wrap rounded-md [overflow-wrap:anywhere] border border-border bg-input px-4 py-3 text-xs leading-5 text-muted">{job.lines.join("\n")}</pre>
            : <div className="rounded-md border border-border bg-input px-4 py-3 text-sm text-subtle">The job wrote no log lines.</div>}
        </section>
      </div>
    </Dialog>
  );
}
