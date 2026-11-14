import { useId, type ReactNode } from "react";

export function DashboardCard({ title, note, action, children }: {
  title: string;
  /** A short live fact beside the title, such as how many jobs run. */
  note?: string;
  action?: ReactNode;
  children: ReactNode;
}) {
  const titleId = useId();
  return (
    <section aria-labelledby={titleId} className="min-w-0 overflow-hidden rounded-lg border border-border bg-card shadow-[0_1.25rem_3.75rem_rgba(0,0,0,0.18)]">
      <div className="flex h-12 items-center justify-between gap-3 border-b border-border px-4">
        <div className="flex min-w-0 items-baseline gap-2">
          <h2 id={titleId} className="text-base font-semibold text-text">{title}</h2>
          {note ? <span className="text-xs font-semibold text-muted">{note}</span> : null}
        </div>
        {action}
      </div>
      {children}
    </section>
  );
}
