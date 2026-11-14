import { createContext, type ReactNode, useContext, useEffect, useRef, useMemo, useState } from "react";
import { useQueries, useQueryClient } from "@tanstack/react-query";
import { getOperationJob, type OperationJobResponse } from "../api";
import { ApiError } from "../backend/error";
import { currentFileText } from "../lib/jobActivity";
import { rememberJobLabel } from "../lib/jobLabels";
import { recentJobsKey } from "./useRecentJobs";

const storageKey = "mkvo.web.activeOperationJob";
const terminalStatuses = new Set(["Completed", "Failed", "Skipped", "Canceled"]);

// A restarted server has forgotten the job this tab remembers; a proxy's bare 404 does not say so.
const isUnknownJob = (error: unknown) => error instanceof ApiError && error.code === "not_found";

type ActiveOperation = { id: string; label: string };

/** A job this tab started, with what the last poll said about it. */
export type TrackedOperation = ActiveOperation & {
  job: OperationJobResponse | undefined;
  isRunning: boolean;
  statusText: string;
};

type OperationJobContextValue = {
  /** Every job this tab follows, newest first: the latest one of each label. */
  operations: TrackedOperation[];
  operationFor: (label: string) => TrackedOperation | null;
  /** The newest job, as `activeOperation`, `job`, `isRunning` and `statusText`. */
  activeOperation: ActiveOperation | null;
  job: OperationJobResponse | undefined;
  isRunning: boolean;
  statusText: string | null;
  trackJob: (id: string, label: string) => void;
};

const OperationJobContext = createContext<OperationJobContextValue | null>(null);

// Earlier versions stored the one job they followed as a bare object.
function readTrackedOperations(): ActiveOperation[] {
  try {
    const stored = JSON.parse(sessionStorage.getItem(storageKey) ?? "[]") as ActiveOperation | ActiveOperation[];
    return Array.isArray(stored) ? stored : [stored];
  } catch {
    return [];
  }
}

function describe(label: string, job: OperationJobResponse | undefined) {
  if (!job) return `${label}: checking job`;
  const finished = job.completed + job.failed + job.skipped;
  if (!terminalStatuses.has(job.status)) {
    const file = currentFileText(job);
    return `${label}: ${finished}/${job.total}${file ? ` · ${file}` : ""}`;
  }
  if (job.status === "Completed") return `${label}: completed ${finished}/${job.total}`;
  if (job.status === "Failed") return `${label}: failed`;
  return `${label}: ${job.status.toLowerCase()}`;
}

export function OperationJobProvider({ children }: { children: ReactNode }) {
  const [tracked, setTracked] = useState(readTrackedOperations);
  const jobQueries = useQueries({
    queries: tracked.map((operation) => ({
      queryKey: ["operation-job", operation.id],
      queryFn: () => getOperationJob(operation.id),
      refetchInterval: (query: { state: { data?: OperationJobResponse; error: unknown } }) => {
        const job = query.state.data;
        return (job && terminalStatuses.has(job.status)) || isUnknownJob(query.state.error) ? false : 1000;
      }
    }))
  });
  // A job the server has forgotten is neither shown nor remembered.
  const unknownIds = tracked.filter((_, index) => isUnknownJob(jobQueries[index]?.error)).map((operation) => operation.id).join("|");

  const jobs = jobQueries.map((query) => query.data);
  const jobsKey = jobs.map((job) => job ? `${job.id}:${job.status}:${job.completed}:${job.failed}:${job.skipped}:${job.currentFile}:${job.currentFilePercent}:${job.lines.length}` : "").join("|");
  const operations = useMemo<TrackedOperation[]>(() => tracked
    .map((operation, index) => {
      const job = jobs[index];
      return { ...operation, job, isRunning: !job || !terminalStatuses.has(job.status), statusText: describe(operation.label, job) };
    })
    .filter((operation) => !unknownIds.split("|").includes(operation.id)),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [tracked, jobsKey, unknownIds]);

  // Nor is one remembered that ended over a day ago: its page has nothing left to show for it.
  useEffect(() => {
    const remembered = operations
      .filter(({ job }) => !(job && terminalStatuses.has(job.status) && Date.now() - new Date(job.completedUtc ?? job.createdUtc).getTime() > 86_400_000))
      .map(({ id, label }) => ({ id, label }));
    try {
      if (remembered.length) sessionStorage.setItem(storageKey, JSON.stringify(remembered));
      else sessionStorage.removeItem(storageKey);
    } catch { /* state still tracks them */ }
  }, [operations]);

  const queryClient = useQueryClient();
  // The jobs seen running. One that was already over when the tab loaded has changed nothing since.
  const runningJobs = useRef(new Set<string>());
  useEffect(() => {
    for (const { id, job } of operations) {
      if (!job) continue;
      if (!terminalStatuses.has(job.status)) { runningJobs.current.add(id); continue; }
      if (!runningJobs.current.delete(id)) continue;
      void queryClient.invalidateQueries({ queryKey: ["current-scan-files"] });
      void queryClient.invalidateQueries({ queryKey: ["propedit-template"] });
      void queryClient.invalidateQueries({ queryKey: recentJobsKey });
    }
  }, [operations, queryClient]);

  const value = useMemo<OperationJobContextValue>(() => {
    const newest = operations[0] ?? null;
    return {
      operations,
      operationFor: (label) => operations.find((operation) => operation.label === label) ?? null,
      activeOperation: newest ? { id: newest.id, label: newest.label } : null,
      job: newest?.job,
      isRunning: Boolean(newest?.isRunning),
      statusText: newest?.statusText ?? null,
      trackJob: (id, label) => {
        setTracked((current) => [{ id, label }, ...current.filter((operation) => operation.label !== label && operation.id !== id)]);
        rememberJobLabel(id, label);
        runningJobs.current.add(id);
        void queryClient.invalidateQueries({ queryKey: recentJobsKey });
      }
    };
  }, [operations, queryClient]);

  return <OperationJobContext.Provider value={value}>{children}</OperationJobContext.Provider>;
}

export function useOperationJob() {
  const value = useContext(OperationJobContext);
  if (!value) throw new Error("useOperationJob must be used inside OperationJobProvider");
  return value;
}
