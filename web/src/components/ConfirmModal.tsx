import type { ReactNode } from "react";

import { Dialog } from "./Dialog";

type ConfirmModalProps = {
  title: string;
  message: string;
  confirmLabel: string;
  onConfirm: () => void;
  onClose: () => void;
  /** The action replaces or deletes files: it reads as a warning and Cancel takes the first focus. */
  danger?: boolean;
  /** Detail under the message, such as the files the action reaches; widens the dialog. */
  children?: ReactNode;
};

export function ConfirmModal({ title, message, confirmLabel, onConfirm, onClose, danger = false, children }: ConfirmModalProps) {
  return (
    <Dialog title={title} onClose={onClose} className={children ? "w-[min(46rem,calc(100vw-3rem))]" : "w-[min(30rem,calc(100vw-3rem))]"}>
      <div className={["min-h-0 flex-1 overflow-y-auto p-5", children ? "" : "pb-0"].join(" ")}>
        <p className={["text-sm text-muted", children ? "leading-6" : "leading-5"].join(" ")}>{message}</p>
        {children}
      </div>
      <div className={["flex shrink-0 justify-end gap-2", children ? "border-t border-border p-4" : "p-5"].join(" ")}>
        <button type="button" onClick={onClose} data-autofocus={danger || undefined} className="h-9 rounded-md border border-border bg-button px-4 text-sm font-semibold text-muted transition hover:bg-button-hover hover:text-text">
          Cancel
        </button>
        <button type="button" onClick={onConfirm} className={danger
          ? "h-9 rounded-md border border-warning bg-button px-4 text-sm font-semibold text-warning transition hover:bg-button-hover"
          : "h-9 rounded-md bg-accent px-4 text-sm font-semibold text-window transition hover:bg-accent-hover"}>
          {confirmLabel}
        </button>
      </div>
    </Dialog>
  );
}
