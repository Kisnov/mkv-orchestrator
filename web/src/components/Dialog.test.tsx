import { useState } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";

import { ConfirmModal } from "./ConfirmModal";
import { Dialog } from "./Dialog";
import { PreviewSummaryModal } from "./PreviewSummaryModal";
import { useOverlayAccessibility } from "./useOverlayAccessibility";

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

function DestructiveHarness() {
  useOverlayAccessibility();
  const [open, setOpen] = useState(false);
  // The real Apply button is disabled while the plan is built, which drops focus to the page.
  return <>
    <button type="button" data-dialog-return onClick={(event) => { event.currentTarget.blur(); setOpen(true); }}>Apply Changes</button>
    {open ? <ConfirmModal danger title="Replace 1 original file?" message="This can't be undone." confirmLabel="Replace 1 file" onConfirm={() => setOpen(false)} onClose={() => setOpen(false)} /> : null}
  </>;
}

it("starts a destructive confirmation on Cancel, styles its action as a warning, and hands focus back to the actions", async () => {
  vi.spyOn(Element.prototype, "getClientRects").mockReturnValue([{}] as unknown as DOMRectList);
  const user = userEvent.setup();
  render(<DestructiveHarness />);

  await user.click(screen.getByRole("button", { name: "Apply Changes" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus());
  expect(screen.getByRole("button", { name: "Replace 1 file" })).toHaveClass("text-warning", "border-warning");
  await user.click(screen.getByRole("button", { name: "Replace 1 file" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Apply Changes" })).toHaveFocus());
  vi.restoreAllMocks();
});
