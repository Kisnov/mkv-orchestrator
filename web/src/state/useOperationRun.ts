import { useEffect, useRef, useState } from "react";
import { useMutation } from "@tanstack/react-query";

import { cancelOperationJob, type OperationJobResponse } from "../api";
import { createLocalId } from "../lib/localId";
import { useOperationJob } from "./OperationJobContext";
import { activeJobStatuses } from "./useRecentJobs";

type Plan = { actions: unknown[]; status: string; planId: string | null; planFingerprint: string | null; idempotencyKey: string | null };
type PlannedRequest = { planId?: string | null; planFingerprint?: string | null; idempotencyKey?: string | null };

export type OperationRunOptions<Request, Result extends Plan> = {
  /** Names the job in the sidebar and tells this page's job apart from another page's. */
  label: string;
  /** The name this page's jobs were stored under before, so one running across an update is still its own. */
  legacyLabel?: string;
  initialStatus: string;
  preview: (request: Request) => Promise<Result>;
  apply: (request: Request & PlannedRequest) => Promise<{ id: string; total: number }>;
  /** The plan as it ran, read from a finished job. */
  resultOf: (job: OperationJobResponse) => Result | null;
  startedText: (total: number) => string;
  noChangesText: string;
  cancelingText?: string;
  /** Apply the previewed plan when the request has not changed since, instead of planning again. */
  reusePreview?: boolean;
  onPreview?: (plan: Result, request: Request) => void;
};

export { activeJobStatuses as activeStatuses };

/** The preview, apply, progress and cancel flow every track operation shares. */
export function useOperationRun<Request, Result extends Plan>(options: OperationRunOptions<Request, Result>) {
  const { label } = options;
  const operation = useOperationJob();
  const [status, setStatus] = useState(options.initialStatus);
  const [previewResult, setPreviewResult] = useState<Result | null>(null);
  const lastPreview = useRef<{ request: string; plan: Result } | null>(null);
  const failed = (fallback: string) => (error: unknown) => setStatus(error instanceof Error ? error.message : fallback);

  const preview = useMutation({
    mutationFn: options.preview,
    onSuccess: (plan, request) => {
      lastPreview.current = { request: JSON.stringify(request), plan };
      setPreviewResult(plan);
      setStatus(plan.status);
      options.onPreview?.(plan, request);
    },
    onError: failed("Preview failed.")
  });
  const apply = useMutation({
    mutationFn: options.apply,
    onSuccess: (job) => {
      operation.trackJob(job.id, label);
      setStatus(options.startedText(job.total));
    },
    onError: failed("Apply failed.")
  });
  const cancel = useMutation({ mutationFn: cancelOperationJob });

  const own = operation.operationFor(label) ?? (options.legacyLabel ? operation.operationFor(options.legacyLabel) : null);
  const job = own?.job;
  const isApplying = apply.isPending || Boolean(own?.isRunning);

  useEffect(() => {
    if (!job) return;
    if (activeJobStatuses.has(job.status)) {
      const file = job.currentFile ? ` | ${job.currentFile} ${job.currentFilePercent}%` : "";
      setStatus(`Applying ${job.completed + job.failed + job.skipped}/${job.total}${file}`);
      return;
    }
    const result = options.resultOf(job);
    if (result) {
      setPreviewResult(result);
      setStatus(result.status);
    } else if (job.status === "Failed") {
      setStatus(job.error || "Apply failed.");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [job?.status, job?.completed, job?.currentFile, job?.currentFilePercent]);

  async function applyPlan(request: Request, plan: Result) {
    // A plan runs once: applying it again would only replay the finished job.
    lastPreview.current = null;
    await apply.mutateAsync({ ...request, planId: plan.planId, planFingerprint: plan.planFingerprint, idempotencyKey: plan.idempotencyKey ?? createLocalId() });
  }

  return {
    status,
    setStatus,
    previewResult,
    setPreviewResult,
    /** This page's tracked job, running or finished. */
    job,
    isApplying,
    isBusy: isApplying || preview.isPending,
    isCanceling: cancel.isPending,
    preview: (request: Request) => preview.mutate(request),
    applyPlan,
    /**
     * Plans (or reuses the previewed plan) and applies it. `confirm` may hold
     * the plan back, for the page to apply later with `applyPlan`.
     */
    async apply(request: Request, confirm?: (plan: Result) => boolean) {
      try {
        const previewed = options.reusePreview && lastPreview.current?.request === JSON.stringify(request) ? lastPreview.current.plan : null;
        const plan = previewed ?? await preview.mutateAsync(request);
        if (!plan.actions.length) { setStatus(options.noChangesText); return; }
        if (confirm && !confirm(plan)) return;
        await applyPlan(request, plan);
      } catch { /* Mutation handlers display the error. */ }
    },
    cancel() {
      if (!own) return;
      cancel.mutate(own.id);
      setStatus(options.cancelingText ?? `Canceling ${label}...`);
    }
  };
}

/** What the shared operation layout reads from a run. */
export type OperationRun = Pick<ReturnType<typeof useOperationRun<unknown, Plan>>, "status" | "job" | "isApplying" | "isBusy" | "isCanceling" | "cancel">;
