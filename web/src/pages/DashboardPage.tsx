import { useEffect, useMemo, useState } from "react";
import { useQueries, useQuery } from "@tanstack/react-query";
import { AlertTriangle, Check } from "lucide-react";
import { Link } from "react-router-dom";

import { getRenameBatches, getStatus, getWatchHealth, getWebSettings, listRecentJobs, testMediaServerConnection, type RecentJob } from "../api";
import { DashboardActivity } from "../components/DashboardActivity";
import { DashboardRenames } from "../components/DashboardRenames";
import { DashboardScannedFiles } from "../components/DashboardScannedFiles";
import { DashboardSystem, type ServerCheck } from "../components/DashboardSystem";
import { rowAction } from "../components/dashboardStyles";
import { SectionHeader } from "../components/SectionHeader";
import { jobName, timeAgo, unresolvedFailures } from "../lib/jobActivity";
import { readDismissedFailures, readJobLabels, rememberDismissedFailure } from "../lib/jobLabels";
import { watchState } from "../lib/systemHealth";
import { useMediaLibrary } from "../state/MediaLibraryContext";
import { useOperationJob } from "../state/OperationJobContext";
import { activeJobStatuses, recentJobsKey } from "../state/useRecentJobs";

type AttentionItem = { id: string; text: string; action?: { label: string; to?: string; onClick?: () => void }; onDismiss?: () => void };

function readScanSources(): string[] {
  try {
    return JSON.parse(sessionStorage.getItem("mkvo.web.scanSources") ?? "[]") as string[];
  } catch {
    return [];
  }
}

/** What MKVO is doing and what needs the user: jobs, scanned files, tools, watcher and media servers. */
export function DashboardPage() {
  // Ages such as "5 minutes ago" move on even when no answer changes.
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  const { files, templateFilePath } = useMediaLibrary();
  const operation = useOperationJob();
  const status = useQuery({ queryKey: ["status"], queryFn: getStatus });
  const settings = useQuery({ queryKey: ["settings"], queryFn: getWebSettings });
  // The sidebar polls this list on every page; here it is only read.
  const recentJobs = useQuery({ queryKey: recentJobsKey, queryFn: () => listRecentJobs(20) });
  // The page's one poll of its own. React Query pauses it while the tab is hidden.
  // Nothing changes while watching is off, so then it is read once.
  const watchingOn = settings.data?.enableLiveWatchFolderMonitoring ?? false;
  const watchHealth = useQuery({ queryKey: ["watch-health"], queryFn: getWatchHealth, refetchInterval: (query) => watchingOn || query.state.data?.running ? 30_000 : false });
  const renameBatches = useQuery({ queryKey: ["rename-batches"], queryFn: getRenameBatches, staleTime: 60_000 });
  // One connection check per server and session; after that only when asked.
  const servers = settings.data?.mediaServers ?? [];
  const serverChecks = useQueries({
    queries: servers.map((server) => ({
      queryKey: ["media-server-check", server.id],
      queryFn: () => testMediaServerConnection({ id: server.id }),
      enabled: server.hasApiKey,
      staleTime: Number.POSITIVE_INFINITY,
      gcTime: Number.POSITIVE_INFINITY,
      retry: false,
      refetchOnReconnect: false
    }))
  });
  const [logJobId, setLogJobId] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(readDismissedFailures);

  const jobs = recentJobs.data?.jobs ?? [];
  // The names this browser remembers, and the jobs it follows now.
  const labels = useMemo(() => new Map([...readJobLabels(), ...operation.operations.map((tracked): [string, string] => [tracked.id, tracked.label])]), [operation.operations]);
  const tools = status.data?.tools ?? [];
  const watch = watchState(watchingOn, watchHealth.data, watchHealth.isError, now);
  const checks: ServerCheck[] = servers.map((server, index) => {
    const check = serverChecks[index];
    const shared = { server, isChecking: check.isFetching, check: () => void check.refetch() };
    if (!server.hasApiKey) return { ...shared, health: "problem", text: "No API key is saved." };
    if (check.data?.success) return { ...shared, health: "ok", text: `Connected, ${check.data.libraryCount} library ${check.data.libraryCount === 1 ? "path" : "paths"}` };
    if (check.data) return { ...shared, health: "problem", text: check.data.status };
    if (check.error) return { ...shared, health: "problem", text: check.error.message };
    return { ...shared, health: "checking", text: "" };
  });

  const failures = unresolvedFailures(jobs, labels, now).filter((job) => !dismissed.includes(job.id));
  const nameOf = (job: RecentJob) => jobName(job, labels);
  const unreadable = files.filter((file) => file.status.toLowerCase().includes("failed")).length;
  const attention: AttentionItem[] = [
    ...tools.filter((tool) => !tool.available).map((tool) => ({ id: `tool-${tool.name}`, text: `${tool.name} is not available: ${tool.error || "not found"}.`, action: { label: "Tool settings", to: "/settings?tab=general" } })),
    ...(watch.health === "problem" ? [{ id: "watch", text: `Watch folders: ${watch.text}`, action: { label: "Watch settings", to: "/settings?tab=library" } }] : []),
    ...checks.filter((check) => check.health === "problem" && !check.isChecking).map((check) => ({ id: `server-${check.server.id}`, text: check.text.startsWith(check.server.name) ? check.text : `${check.server.name}: ${check.text}`, action: { label: "Server settings", to: "/settings?tab=library" } })),
    ...failures.map((job) => ({
      id: `job-${job.id}`,
      text: `${nameOf(job)} failed ${timeAgo(job.completedUtc ?? job.createdUtc, now)}.`,
      action: job.kind === "scan" ? { label: "Open Scan", to: "/dashboard/scan" } : { label: "View log", onClick: () => setLogJobId(job.id) },
      onDismiss: () => { rememberDismissedFailure(job.id); setDismissed((current) => [...current, job.id]); }
    })),
    ...(unreadable ? [{ id: "unreadable", text: `${unreadable} scanned ${unreadable === 1 ? "file" : "files"} could not be read.`, action: { label: "Open Scan", to: "/dashboard/scan" } }] : [])
  ];
  const stillChecking = status.isLoading || recentJobs.isLoading || checks.some((check) => check.isChecking);

  return (
    <div className="dashboard-page @container pb-2">
      <SectionHeader title="Dashboard" description="Jobs, scanned files and the state of your tools." />
      <section aria-label="Needs attention" aria-live="polite" className={["rounded-lg border px-4 py-2.5", attention.length ? "border-warning bg-input" : "border-border bg-card"].join(" ")}>
        {attention.length === 0 ? (
          <p className="flex items-center gap-2 text-sm text-muted">
            {stillChecking ? "Checking jobs, tools and servers." : <><Check size={16} className="shrink-0 text-success" aria-hidden="true" />Nothing needs your attention.</>}
          </p>
        ) : (
          <ul className="space-y-1.5">
            {attention.map((item) => (
              <li key={item.id} className="grid grid-cols-[0.875rem_minmax(0,1fr)] gap-x-3 text-sm">
                <AlertTriangle size={14} className="mt-1 text-warning" aria-hidden="true" />
                <div className="flex min-w-0 flex-wrap items-baseline justify-between gap-x-4 gap-y-0.5">
                  <span className="min-w-0 break-words text-text [overflow-wrap:anywhere]">{item.text}</span>
                  <span className="flex items-baseline gap-x-4 text-xs font-semibold">
                    {item.action?.to ? <Link to={item.action.to} className={rowAction}>{item.action.label}</Link> : null}
                    {item.action?.onClick ? <button type="button" onClick={item.action.onClick} className={rowAction}>{item.action.label}</button> : null}
                    {item.onDismiss ? <button type="button" onClick={item.onDismiss} className={rowAction} aria-label={`Dismiss: ${item.text}`}>Dismiss</button> : null}
                  </span>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>
      <div className="mt-4 grid items-start gap-4 @4xl:grid-cols-[minmax(0,1.45fr)_minmax(0,1fr)]">
        <DashboardActivity jobs={jobs} labels={labels} now={now} isLoading={recentJobs.isLoading} error={recentJobs.error} onRetry={() => void recentJobs.refetch()} logJobId={logJobId} onLogJobChange={setLogJobId} />
        <div className="grid min-w-0 gap-4">
          <DashboardScannedFiles files={files} templateFilePath={templateFilePath} sources={files.length ? readScanSources() : []} isScanning={jobs.some((job) => job.kind === "scan" && activeJobStatuses.has(job.status))} />
          <DashboardSystem tools={tools} toolsLoading={status.isLoading} watch={watch} servers={checks} />
          {renameBatches.data?.batches.length ? <DashboardRenames batches={renameBatches.data.batches} now={now} /> : null}
        </div>
      </div>
    </div>
  );
}
