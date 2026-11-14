import { useId, type ReactNode } from "react";
import { X } from "lucide-react";

// Declares its own semantics; focus trap and Escape come from useOverlayAccessibility, which Layout mounts.
export function Dialog({ title, onClose, className, children }: {
  title: string;
  onClose: () => void;
  /** Width and height of the panel. */
  className: string;
  children: ReactNode;
}) {
  const titleId = useId();
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-6">
      <section role="dialog" aria-modal="true" aria-labelledby={titleId} className={`flex max-h-[calc(100vh-3rem)] flex-col overflow-hidden rounded-lg border-2 border-window bg-card shadow-[0_1.875rem_5.625rem_rgba(0,0,0,0.55)] ${className}`}>
        <div className="flex h-10 shrink-0 items-center justify-between border-b border-border bg-window px-4">
          <div id={titleId} className="text-sm font-semibold text-muted">{title}</div>
          <button type="button" onClick={onClose} className="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted transition hover:bg-button-hover hover:text-text" title="Close" aria-label="Close">
            <X size={16} />
          </button>
        </div>
        {children}
      </section>
    </div>
  );
}
