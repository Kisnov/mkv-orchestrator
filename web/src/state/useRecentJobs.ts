import { useQuery } from "@tanstack/react-query";

import { listRecentJobs } from "../api";

export const recentJobsKey = ["recent-jobs"];
export const activeJobStatuses = new Set(["Running", "Queued", "WaitingForResources", "Canceling"]);

/**
 * The backend's job list, shared by the sidebar and the Dashboard: one small
 * request, quicker while a job runs. React Query pauses it in a hidden tab.
 */
export function useRecentJobs(idleIntervalMs: number) {
  return useQuery({
    queryKey: recentJobsKey,
    queryFn: () => listRecentJobs(20),
    refetchInterval: (query) => query.state.data?.jobs.some((job) => activeJobStatuses.has(job.status)) ? 2000 : idleIntervalMs
  });
}
