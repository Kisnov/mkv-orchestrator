import { useEffect, useRef, useState, type ComponentProps, type ReactNode } from "react";
import { PanelLeftClose, PanelLeftOpen } from "lucide-react";

import { activeStatuses, type OperationRun } from "../state/useOperationRun";
import { JobLogModal } from "./JobLogModal";
import { MobileOptions } from "./MobileOptions";
import { PanelDivider } from "./PanelDivider";
import { PreviewSummaryModal } from "./PreviewSummaryModal";
import { SectionHeader } from "./SectionHeader";

const optionsHiddenKey = "mkvo.operationOptions.hidden";
const optionsId = "operation-options";

function readOptionsHidden() {
  try { return localStorage.getItem(optionsHiddenKey) === "true"; } catch { return false; }
}

const panel = "flex min-h-0 min-w-0 flex-col rounded-lg border border-border bg-card p-4 shadow-[0_1.25rem_3.75rem_rgba(0,0,0,0.18)]";

/**
 * The layout every track operation shares: options and actions on the left,
 * the file list above the details on the right, and the preview summary and
 * job log dialogs.
 */
export function OperationShell({ title, description, options, run, canRun, onPreview, onApply, selectedText, hiddenSelected = 0, notice, files, details, initialSplit = 56, summary, children }: {
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
  /** Selected files the table's filter hides; they are still sent. */
  hiddenSelected?: number;
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
  const [optionsHidden, setOptionsHidden] = useState(readOptionsHidden);
  const panelRef = useRef<HTMLDivElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);
  const toggled = useRef(false);
  const finishedJob = run.job && !activeStatuses.has(run.job.status) ? run.job : undefined;

  // The toggle moves between the panel and the toolbar, so focus follows it.
  useEffect(() => {
    if (toggled.current) toggleRef.current?.focus();
  }, [optionsHidden]);

  function toggleOptions() {
    toggled.current = true;
    setOptionsHidden(!optionsHidden);
    try { localStorage.setItem(optionsHiddenKey, String(!optionsHidden)); } catch { /* the choice lasts for this page only */ }
  }

  const toggle = (
    <button ref={toggleRef} type="button" onClick={toggleOptions} aria-expanded={!optionsHidden} aria-controls={optionsHidden ? undefined : optionsId} aria-label={optionsHidden ? "Show options" : "Hide options"} title={optionsHidden ? "Show options" : "Hide options"} className={["options-panel-toggle inline-flex h-9 shrink-0 self-start items-center justify-center gap-2 rounded-md border border-border bg-button text-muted transition hover:bg-button-hover hover:text-text", optionsHidden ? "px-3 text-xs font-semibold" : "w-9"].join(" ")}>
      {optionsHidden ? <><PanelLeftOpen size={16} /><span>Options</span></> : <PanelLeftClose size={16} />}
    </button>
  );
  const actions = (
    <div className={optionsHidden ? "flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border border-border bg-card p-3 shadow-[0_1.25rem_3.75rem_rgba(0,0,0,0.18)]" : "mt-3 flex shrink-0 flex-wrap items-center gap-2 border-t border-border pt-3"} aria-label="Operation actions">
      {optionsHidden ? toggle : null}
      <button type="button" onClick={() => { setSummaryOpen(true); onPreview(); }} disabled={run.isBusy || !canRun} className={`h-9 min-w-0 whitespace-nowrap rounded-md border border-border bg-button text-xs font-semibold disabled:text-disabled ${optionsHidden ? "px-4" : "flex-1 px-1.5"}`}>Preview Summary</button>
      {run.isApplying
        ? <button type="button" data-dialog-return onClick={run.cancel} disabled={run.isCanceling} className={`h-9 min-w-0 whitespace-nowrap rounded-md border border-warning bg-button px-3 text-sm text-warning ${optionsHidden ? "" : "flex-1"}`}>Cancel</button>
        : <button type="button" data-dialog-return onClick={onApply} disabled={run.isBusy || !canRun} className={`h-9 min-w-0 whitespace-nowrap rounded-md bg-accent text-xs font-semibold disabled:bg-button disabled:text-disabled ${optionsHidden ? "px-4" : "flex-1 px-1.5"}`}>Apply Changes</button>}
      <span className={`text-xs text-muted ${optionsHidden ? "" : "w-full"}`}>{selectedText}{hiddenSelected > 0 ? ` (${hiddenSelected} hidden)` : ""}</span>
      {notice}
      <span role="status" className={`min-w-0 break-words text-xs text-muted [overflow-wrap:anywhere] ${optionsHidden ? "flex-1" : "w-full"}`}>{run.status}</span>
      {finishedJob ? (
        <button type="button" onClick={() => setLogOpen(true)} className={["text-xs font-semibold hover:underline", finishedJob.failed > 0 || finishedJob.status === "Failed" ? "text-warning" : "text-muted"].join(" ")}>
          {finishedJob.failed > 0 ? `Show the ${finishedJob.failed} failed file(s) and job log` : "Show job log"}
        </button>
      ) : null}
    </div>
  );
  const panels = (
    <div ref={panelRef} className="operation-panels grid min-h-0 min-w-0 flex-1" style={{ gridTemplateRows: `minmax(0,${split}fr) 12px minmax(0,${100 - split}fr)` }}>
      <section className={panel}>{files}</section>
      <PanelDivider containerRef={panelRef} value={split} onChange={setSplit} />
      <section className={panel}>{details}</section>
    </div>
  );

  return (
    <div className="workspace-page flex h-full min-h-0 flex-col">
      <SectionHeader title={title} description={description} />
      {optionsHidden ? (
        <div className="flex min-h-0 min-w-0 flex-1 flex-col gap-3">
          {actions}
          {panels}
        </div>
      ) : (
        <div className="grid min-h-0 min-w-0 flex-1 grid-cols-[minmax(14rem,18.75rem)_minmax(0,1fr)] gap-3">
          <section id={optionsId} className="operation-sidebar flex min-h-0 flex-col overflow-hidden rounded-lg border border-border bg-card p-3 shadow-[0_1.25rem_3.75rem_rgba(0,0,0,0.18)]">
            <div className="flex min-h-0 flex-1 gap-2">
              <MobileOptions>{options}</MobileOptions>
              {toggle}
            </div>
            {actions}
          </section>
          {panels}
        </div>
      )}
      {summaryOpen ? <PreviewSummaryModal {...summary} onClose={() => setSummaryOpen(false)} /> : null}
      {logOpen && finishedJob ? <JobLogModal title={`${title} job log`} job={finishedJob} onClose={() => setLogOpen(false)} /> : null}
      {children}
    </div>
  );
}
