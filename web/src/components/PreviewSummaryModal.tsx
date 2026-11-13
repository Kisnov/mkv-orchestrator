import { Copy } from "lucide-react";

import { copyText } from "../lib/copyText";
import { Dialog } from "./Dialog";

export type PreviewSummaryMetric = {
  label: string;
  value: number;
  tone?: string;
};

export type PreviewSummaryRow = {
  key: string;
  title: string;
  detail: string;
  meta?: string;
};

export type PreviewSummarySection = {
  title: string;
  emptyText: string;
  rows: PreviewSummaryRow[];
};

export function PreviewSummaryModal({ title, emptyText, available, status, summary, metrics, sections, onClose }: {
  title: string;
  emptyText: string;
  available: boolean;
  status: string;
  summary: string;
  metrics: PreviewSummaryMetric[];
  sections: PreviewSummarySection[];
  onClose: () => void;
}) {
  const copyContent = async () => {
    const details = [
      summary || status,
      ...sections.flatMap((section) => section.rows.map((row) =>
        `[${section.title.toUpperCase()}] ${row.title}\n${row.detail}${row.meta ? `\n${row.meta}` : ""}`
      ))
    ];
    await copyText(details.join("\n\n"));
  };

  return (
    <Dialog title={title} onClose={onClose} className="h-[min(47.5rem,calc(100vh-3rem))] w-[min(72rem,calc(100vw-3rem))]">
        <div className="min-h-0 flex-1 overflow-y-auto p-5">
          {!available ? (
            <div className="rounded-md border border-border bg-input p-5 text-sm text-muted">{emptyText}</div>
          ) : (
            <>
              <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
                {metrics.map((metric) => (
                  <div key={metric.label} className="rounded-md border border-border bg-panel p-3">
                    <div className="text-xs text-muted">{metric.label}</div>
                    <div className={`mt-1 text-2xl font-semibold ${metric.tone ?? "text-text"}`}>{metric.value}</div>
                  </div>
                ))}
              </div>
              <div className="mt-3 flex items-center justify-between gap-3 rounded-md border border-border bg-input px-4 py-3">
                <div>
                  <div className="text-xs font-semibold uppercase tracking-wide text-subtle">Overall result</div>
                  <div className="mt-1 text-sm text-text">{status || summary}</div>
                </div>
                <button type="button" onClick={copyContent} className="inline-flex h-9 shrink-0 items-center gap-2 rounded-md border border-border bg-button px-3 text-sm font-semibold text-muted hover:bg-button-hover hover:text-text">
                  <Copy size={15} /> Copy details
                </button>
              </div>
              <div className="mt-5 space-y-5">
                {sections.map((section) => (
                  <section key={section.title}>
                    <h3 className="mb-2 text-sm font-semibold text-text">{section.title} <span className="text-muted">({section.rows.length})</span></h3>
                    <div className="overflow-hidden rounded-md border border-border bg-input">
                      {section.rows.length ? section.rows.map((row) => (
                        <div key={row.key} className="border-b border-border px-4 py-3 last:border-0">
                          <div className="font-semibold text-text">{row.title}</div>
                          <div className="mt-1 whitespace-pre-wrap text-xs leading-5 text-muted">{row.detail}</div>
                          {row.meta ? <div className="mt-1 text-xs text-subtle">{row.meta}</div> : null}
                        </div>
                      )) : <div className="px-4 py-3 text-sm text-subtle">{section.emptyText}</div>}
                    </div>
                  </section>
                ))}
              </div>
            </>
          )}
        </div>
    </Dialog>
  );
}
