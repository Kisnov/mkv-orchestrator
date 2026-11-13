import { useRef, useState, type ComponentProps, type ReactNode } from "react";

import { activeStatuses, type OperationRun } from "../state/useOperationRun";
import { JobLogModal } from "./JobLogModal";
import { MobileOptions } from "./MobileOptions";
import { PanelDivider } from "./PanelDivider";
import { PreviewSummaryModal } from "./PreviewSummaryModal";
import { SectionHeader } from "./SectionHeader";

const panel = "flex min-h-0 min-w-0 flex-col rounded-lg border border-border bg-card p-4 shadow-[0_1.25rem_3.75rem_rgba(0,0,0,0.18)]";

/**
 * The layout every track operation shares: options and actions on the left,
 * the file list above the details on the right, and the preview summary and
 * job log dialogs.
 */
export function OperationShell({ title, description, options, run, canRun, onPreview, onApply, selectedText, notice, files, details, initialSplit = 56, summary, children }: {
  title: string;
  description: string;
  /** The sidebar's operation options. */
  options: ReactNode;
  run: OperationRun;
  /** Whether the selection and options make a request worth sending. */
  canRun: boolean;
  onPreview: () => void;
  onApply: () => void;
  selectedText: string;
  /** Says why the operation cannot run, under the selected count. */
  notice?: ReactNode;
  files: ReactNode;
  details: ReactNode;
  /** Percent of the right column the file list starts with. */
  initialSplit?: number;
  summary: Omit<ComponentProps<typeof PreviewSummaryModal>, "onClose">;
  /** Page-specific dialogs. */
  children?: ReactNode;
}) {
  const [split, setSplit] = useState(initialSplit);
  const [summaryOpen, setSummaryOpen] = useState(false);
  const [logOpen, setLogOpen] = useState(false);
  const panelRef = useRef<HTMLDivElement>(null);
  const finishedJob = run.job && !activeStatuses.has(run.job.status) ? run.job : undefined;

  return (
    <div className="workspace-page flex h-full min-h-0 flex-col">
      <SectionHeader title={title} description={description} />
      <div className="grid min-h-0 min-w-0 flex-1 grid-cols-[minmax(14rem,18.75rem)_minmax(0,1fr)] gap-3">
        <section className="operation-sidebar flex min-h-0 flex-col overflow-hidden rounded-lg border border-border bg-card p-3 shadow-[0_1.25rem_3.75rem_rgba(0,0,0,0.18)]">
          <MobileOptions>{options}</MobileOptions>
          <div className="mt-3 flex shrink-0 flex-wrap items-center gap-2 border-t border-border pt-3" aria-label="Operation actions">
            <button type="button" onClick={() => { setSummaryOpen(true); onPreview(); }} disabled={run.isBusy || !canRun} className="h-9 min-w-0 flex-1 whitespace-nowrap rounded-md border border-border bg-button px-1.5 text-xs font-semibold disabled:text-disabled">Preview Summary</button>
            {run.isApplying
              ? <button type="button" onClick={run.cancel} disabled={run.isCanceling} className="h-9 min-w-0 flex-1 whitespace-nowrap rounded-md border border-warning bg-button px-3 text-sm text-warning">Cancel</button>
              : <button type="button" onClick={onApply} disabled={run.isBusy || !canRun} className="h-9 min-w-0 flex-1 whitespace-nowrap rounded-md bg-accent px-1.5 text-xs font-semibold disabled:bg-button disabled:text-disabled">Apply Changes</button>}
            <span className="w-full text-xs text-muted">{selectedText}</span>
            {notice}
            <span role="status" className="w-full min-w-0 break-words text-xs text-muted [overflow-wrap:anywhere]">{run.status}</span>
            {finishedJob ? (
              <button type="button" onClick={() => setLogOpen(true)} className={["text-xs font-semibold hover:underline", finishedJob.failed > 0 || finishedJob.status === "Failed" ? "text-warning" : "text-muted"].join(" ")}>
                {finishedJob.failed > 0 ? `Show the ${finishedJob.failed} failed file(s) and job log` : "Show job log"}
              </button>
            ) : null}
          </div>
        </section>

        <div ref={panelRef} className="grid min-h-0 min-w-0" style={{ gridTemplateRows: `minmax(0,${split}fr) 12px minmax(0,${100 - split}fr)` }}>
          <section className={panel}>{files}</section>
          <PanelDivider containerRef={panelRef} value={split} onChange={setSplit} />
          <section className={panel}>{details}</section>
        </div>
      </div>
      {summaryOpen ? <PreviewSummaryModal {...summary} onClose={() => setSummaryOpen(false)} /> : null}
      {logOpen && finishedJob ? <JobLogModal title={`${title} job log`} job={finishedJob} onClose={() => setLogOpen(false)} /> : null}
      {children}
    </div>
  );
}
