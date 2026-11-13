import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";

import { ConfirmModal } from "./ConfirmModal";
import { Dialog } from "./Dialog";
import { PreviewSummaryModal } from "./PreviewSummaryModal";

// No Layout here, so nothing adds the semantics after the fact.
it("is a labelled modal dialog from its first render", async () => {
  const onClose = vi.fn();
  const user = userEvent.setup();
  render(<Dialog title="Job log" onClose={onClose} className="w-96"><p>Body</p></Dialog>);

  const dialog = screen.getByRole("dialog", { name: "Job log" });
  expect(dialog).toHaveAttribute("aria-modal", "true");
  await user.click(screen.getByRole("button", { name: "Close" }));
  expect(onClose).toHaveBeenCalledTimes(1);
});

it("gives the preview summary and the confirmation the same semantics", () => {
  render(
    <>
      <PreviewSummaryModal title="Reorder Tracks Preview Summary" emptyText="Nothing yet" available={false} status="" summary="" metrics={[]} sections={[]} onClose={() => undefined} />
      <ConfirmModal title="Apply to files that differ?" message="Check them." confirmLabel="Apply" onConfirm={() => undefined} onClose={() => undefined}>
        <p>Episode 10.mkv</p>
      </ConfirmModal>
    </>
  );

  expect(screen.getByRole("dialog", { name: "Reorder Tracks Preview Summary" })).toHaveAttribute("aria-modal", "true");
  const confirm = screen.getByRole("dialog", { name: "Apply to files that differ?" });
  expect(confirm).toHaveTextContent("Episode 10.mkv");
});

it("keeps the roomier layout for a confirmation that lists what it reaches", () => {
  render(<ConfirmModal title="Apply?" message="Check them." confirmLabel="Apply" onConfirm={() => undefined} onClose={() => undefined}><p>Episode 10.mkv</p></ConfirmModal>);

  expect(screen.getByText("Check them.")).toHaveClass("leading-6");
  expect(screen.getByRole("button", { name: "Apply" }).parentElement).toHaveClass("border-t");
});
