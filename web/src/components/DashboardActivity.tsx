import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { AlertTriangle, Ban, Check, Clock, Loader2 } from "lucide-react";
import { Link } from "react-router-dom";

import { ApiError, cancelOperationJob, cancelScan, getOperationJob, type RecentJob } from "../api";
import { currentFileText, duration, jobErrorText, jobFailed, jobFinishedCount, jobName, jobPage, jobResultText, timeAgo } from "../lib/jobActivity";
import { activeJobStatuses, recentJobsKey } from "../state/useRecentJobs";
import { DashboardCard } from "./DashboardCard";
import { linkButton, rowAction } from "./dashboardStyles";
import { JobLogModal } from "./JobLogModal";

const shownAtFirst = 8;

function StatusIcon({ job }: { job: RecentJob }) {
  if (job.status === "Running" || job.status === "Canceling") return <Loader2 size={16} className="animate-spin text-accent" aria-hidden="true" />;
  if (activeJobStatuses.has(job.status)) return <Clock size={16} className="text-muted" aria-hidden="true" />;
  if (jobFailed(job)) return <AlertTriangle size={16} className="text-warning" aria-hidden="true" />;
  if (job.status === "Completed") return <Check size={16} className="text-success" aria-hidden="true" />;
  return <Ban size={16} className="text-muted" aria-hidden="true" />;
}

/**
 * Running and recent jobs from the backend's list. Scans keep their file list
 * out of it, so only the other jobs have a log to open.
 */
export function DashboardActivity({ jobs, labels, now, isLoading, error, onRetry, logJobId, onLogJobChange }: {
  jobs: RecentJob[];
  /** Job id to the name of the page that started it, for the jobs this tab knows. */
  labels: Map<string, string>;
  now: number;
  isLoading: boolean;
  error: Error | null;
  onRetry: () => void;
  /** The job whose log is open; the page owns it so other widgets can open one. */
  logJobId: string | null;
  onLogJobChange: (id: string | null) => void;
}) {
  const queryClient = useQueryClient();
  const [showAll, setShowAll] = useState(false);
  const cancel = useMutation({
    mutationFn: (job: RecentJob): Promise<unknown> => job.kind === "scan" ? cancelScan(job.id) : cancelOperationJob(job.id),
    onSettled: () => queryClient.invalidateQueries({ queryKey: recentJobsKey })
  });
  // A job the server no longer runs is as good as canceled; any other failure shows until the list says the job is over.
  const cancelGone = cancel.error instanceof ApiError && cancel.error.code === "not_found";
  const uncanceled = cancel.isError && !cancelGone ? jobs.find((job) => job.id === cancel.variables?.id && activeJobStatuses.has(job.status)) : undefined;
  const log = useQuery({ queryKey: ["job-log", logJobId], queryFn: () => getOperationJob(logJobId!), enabled: logJobId !== null, retry: false });

  const ordered = [...jobs.filter((job) => activeJobStatuses.has(job.status)), ...jobs.filter((job) => !activeJobStatuses.has(job.status))];
  const shown = showAll ? ordered : ordered.slice(0, shownAtFirst);
  const running = ordered.filter((job) => activeJobStatuses.has(job.status)).length;
  const nameOf = (job: RecentJob) => jobName(job, labels);
  const logJob = jobs.find((job) => job.id === logJobId);

  return (
    <DashboardCard title="Activity" note={running ? `${running} running` : undefined} action={<Link to="/logs" className={rowAction}>Open Logs</Link>}>
      {error && jobs.length === 0 ? (
        <div className="px-4 py-6">
          <p className="text-sm text-warning">Jobs could not be loaded: {error.message}</p>
          {/* index.css makes buttons inherit their font, so the wrapper carries the size. */}
          <div className="mt-3 text-xs font-semibold"><button type="button" onClick={onRetry} className={linkButton}>Try again</button></div>
        </div>
      ) : isLoading ? (
        <div className="flex items-center gap-2 px-4 py-6 text-sm text-muted"><Loader2 size={16} className="animate-spin text-accent" aria-hidden="true" />Loading jobs</div>
      ) : jobs.length === 0 ? (
        <div className="px-4 py-8 text-center">
          <p className="text-sm font-semibold text-text">No jobs yet</p>
          <p className="mx-auto mt-1 max-w-sm text-sm text-muted">Scans and track operations appear here while they run and after they finish.</p>
        </div>
      ) : (
        <ul aria-label="Running and recent jobs">
          {shown.map((job) => {
            const label = nameOf(job);
            const active = activeJobStatuses.has(job.status);
            const failed = jobFailed(job);
            const percent = job.total ? Math.min(100, Math.round(((jobFinishedCount(job) + job.currentFilePercent / 100) / job.total) * 100)) : 0;
            const detail = active
              ? currentFileText(job)
              : jobErrorText(job) || job.summary;
            const page = jobPage(job, labels);
            const logFailed = logJobId === job.id && log.isError;
            return (
              <li key={job.id} className="relative flex flex-wrap items-start gap-x-3 gap-y-1 border-b border-border px-4 py-2.5 last:border-b-0">
                <span className="mt-0.5 shrink-0"><StatusIcon job={job} /></span>
                <div className="min-w-0 flex-1 basis-56">
                  <div className="flex flex-wrap items-baseline gap-x-2">
                    <span className="text-sm font-semibold text-text">{label}</span>
                    <span className={["text-sm tabular-nums", failed ? "font-semibold text-text" : "text-muted"].join(" ")}>{jobResultText(job)}</span>
                  </div>
                  {detail ? <div className="mt-0.5 truncate text-xs text-muted" title={detail}>{detail}</div> : null}
                  {logFailed ? <div role="alert" className="mt-0.5 text-xs text-warning">This job's log is no longer available.</div> : null}
                </div>
                <div className="flex shrink-0 flex-col items-end gap-1 max-md:w-full max-md:flex-row max-md:flex-wrap max-md:items-center max-md:justify-between max-md:gap-x-3 max-md:pl-7">
                  <span className="whitespace-nowrap text-xs tabular-nums text-muted" title={new Date(job.completedUtc ?? job.startedUtc ?? job.createdUtc).toLocaleString()}>
                    {active ? `started ${timeAgo(job.startedUtc ?? job.createdUtc, now)}` : [duration(job.startedUtc, job.completedUtc), timeAgo(job.completedUtc ?? job.createdUtc, now)].filter(Boolean).join(", ")}
                  </span>
                  <span className="flex flex-wrap items-center justify-end gap-x-3 text-xs font-semibold">
                    {failed && page ? <Link to={page.to} className={rowAction} aria-label={`Retry in ${page.label}: ${label}, ${jobResultText(job)}`}>Retry in {page.label}</Link> : null}
                    {job.kind !== "scan" && !active ? <button type="button" onClick={() => onLogJobChange(job.id)} className={rowAction} aria-label={`View log of ${label}, ${jobResultText(job)}`}>View log</button> : null}
                    {active ? (
                      <button type="button" onClick={() => cancel.mutate(job)} disabled={job.status === "Canceling" || (cancel.isPending && cancel.variables?.id === job.id)} aria-label={`Cancel ${label}`} title={job.kind === "property_edit" ? "Stops after the current file" : undefined} className="h-7 rounded-md border border-warning bg-button px-2.5 text-xs font-semibold text-warning disabled:border-border disabled:text-disabled">
                        Cancel
                      </button>
                    ) : null}
                  </span>
                </div>
                {active ? (
                  <div role="progressbar" aria-label={`${label} progress`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={percent} className="absolute inset-x-0 bottom-0 h-0.5 bg-border">
                    <div className="h-full bg-accent transition-[width] duration-500" style={{ width: `${percent}%` }} />
                  </div>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
      {ordered.length > shownAtFirst ? (
        <div className="border-t border-border px-4 py-2 text-xs font-semibold">
          <button type="button" onClick={() => setShowAll(!showAll)} aria-expanded={showAll} className={rowAction}>
            {showAll ? "Show fewer" : `Show ${ordered.length - shownAtFirst} more`}
          </button>
        </div>
      ) : null}
      {uncanceled ? <p role="alert" className="border-t border-border px-4 py-2 text-xs text-warning">{nameOf(uncanceled)} could not be canceled: {cancel.error?.message}</p> : null}
      {logJobId && log.data ? <JobLogModal title={`${logJob ? nameOf(logJob) : "Job"} log`} job={log.data} onClose={() => onLogJobChange(null)} /> : null}
    </DashboardCard>
  );
}
