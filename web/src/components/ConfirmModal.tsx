import { X } from "lucide-react";

type ConfirmModalProps = {
  title: string;
  message: string;
  confirmLabel: string;
  onConfirm: () => void;
  onClose: () => void;
};

export function ConfirmModal({ title, message, confirmLabel, onConfirm, onClose }: ConfirmModalProps) {
  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/55 p-6">
      <section role="dialog" aria-modal="true" aria-label={title} className="w-[min(30rem,calc(100vw-3rem))] rounded-lg border-2 border-window bg-card shadow-[0_1.875rem_5.625rem_rgba(0,0,0,0.55)]">
        <div className="flex h-10 items-center justify-between border-b border-border bg-window px-4">
          <div className="text-sm font-semibold text-muted">{title}</div>
          <button type="button" onClick={onClose} className="inline-flex h-7 w-7 items-center justify-center rounded-md text-muted transition hover:bg-button-hover hover:text-text" title="Close" aria-label="Close">
            <X size={16} />
          </button>
        </div>
        <div className="p-5">
          <p className="text-sm leading-5 text-muted">{message}</p>
          <div className="mt-5 flex justify-end gap-2">
            <button type="button" onClick={onClose} className="h-9 rounded-md border border-border bg-button px-4 text-sm font-semibold text-muted transition hover:bg-button-hover hover:text-text">
              Cancel
            </button>
            <button type="button" onClick={onConfirm} className="h-9 rounded-md bg-accent px-4 text-sm font-semibold text-window transition hover:bg-accent-hover">
              {confirmLabel}
            </button>
          </div>
        </div>
      </section>
    </div>
  );
}
