import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";

import type { OperationJobResponse } from "../api";
import type { OperationRun } from "../state/useOperationRun";
import { OperationShell } from "./OperationShell";

function job(patch: Partial<OperationJobResponse>): OperationJobResponse {
  return {
    id: "job-1", kind: "Remux", status: "Completed", createdUtc: "", startedUtc: null, completedUtc: null,
    completed: 3, failed: 0, skipped: 0, total: 3, currentFile: "", currentFilePercent: 0, lines: [], muxResult: null, propEditResult: null, error: "",
    ...patch
  };
}

const idle: OperationRun = { status: "Pick files.", job: undefined, isApplying: false, isBusy: false, isCanceling: false, cancel: () => undefined };

function shell(run: OperationRun, props: Partial<{ canRun: boolean; onPreview: () => void; onApply: () => void }> = {}) {
  return render(
    <OperationShell
      title="Reorder Tracks" description="Reorder tracks." options={<h2>Reorder Options</h2>} run={run}
      canRun={props.canRun ?? true} onPreview={props.onPreview ?? (() => undefined)} onApply={props.onApply ?? (() => undefined)}
      selectedText="2 files selected" notice={<span>The template is not in the batch.</span>}
      files={<h2>File Info</h2>} details={<h2>Template Track Order</h2>}
      summary={{ title: "Reorder Tracks Preview Summary", emptyText: "Build a preview.", available: false, status: "", summary: "", metrics: [], sections: [] }}
    />
  );
}

it("lays out options, actions, status, files and details", async () => {
  const onPreview = vi.fn();
  const onApply = vi.fn();
  const user = userEvent.setup();
  shell(idle, { onPreview, onApply });

  expect(screen.getByRole("heading", { name: "Reorder Tracks" })).toBeInTheDocument();
  for (const name of ["Reorder Options", "File Info", "Template Track Order"]) expect(screen.getByRole("heading", { name })).toBeInTheDocument();
  expect(screen.getByText("2 files selected")).toBeInTheDocument();
  expect(screen.getByText("The template is not in the batch.")).toBeInTheDocument();
  expect(screen.getByRole("status")).toHaveTextContent("Pick files.");
  expect(screen.getByRole("separator", { name: "Resize file and details panels" })).toHaveAttribute("aria-valuenow", "56");

  await user.click(screen.getByRole("button", { name: "Apply Changes" }));
  expect(onApply).toHaveBeenCalledTimes(1);
  await user.click(screen.getByRole("button", { name: "Preview Summary" }));
  expect(onPreview).toHaveBeenCalledTimes(1);
  expect(screen.getByRole("dialog", { name: "Reorder Tracks Preview Summary" })).toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Close" }));
  expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
});

it("disables both actions while the request is not worth sending or a run is busy", () => {
  const { unmount } = shell(idle, { canRun: false });
  expect(screen.getByRole("button", { name: "Preview Summary" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Apply Changes" })).toBeDisabled();
  unmount();

  shell({ ...idle, isBusy: true });
  expect(screen.getByRole("button", { name: "Preview Summary" })).toBeDisabled();
  expect(screen.getByRole("button", { name: "Apply Changes" })).toBeDisabled();
});

it("offers Cancel while applying and no job log until the job ends", async () => {
  const cancel = vi.fn();
  const user = userEvent.setup();
  shell({ ...idle, isApplying: true, isBusy: true, cancel, job: job({ status: "Running", completed: 1 }) });

  expect(screen.queryByRole("button", { name: "Apply Changes" })).not.toBeInTheDocument();
  await user.click(screen.getByRole("button", { name: "Cancel" }));
  expect(cancel).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("button", { name: /job log/ })).not.toBeInTheDocument();
});

// The exact strings batch_failure produces (remux_workflow.rs, a_batch_failure_names_the_failed_files_and_ignores_canceled_ones).
it("shows a finished job's failed files and log lines", async () => {
  const user = userEvent.setup();
  shell({ ...idle, job: job({
    status: "Failed", completed: 1, failed: 2, total: 4,
    error: "internal error: 2 of 4 files failed: a.mkv: tool failed; b.mkv: tool failed",
    lines: ["Remux /media/a.mkv", "/media/a.mkv: tool failed", "/media/b.mkv: tool failed"]
  }) });

  await user.click(screen.getByRole("button", { name: "Show the 2 failed file(s) and job log" }));
  const dialog = screen.getByRole("dialog", { name: "Reorder Tracks job log" });
  expect(within(dialog).getByText("1 done, 2 failed, 0 skipped of 4")).toBeInTheDocument();
  const failed = within(dialog).getAllByRole("listitem").map((item) => item.textContent);
  expect(failed).toEqual(["a.mkv: tool failed", "b.mkv: tool failed"]);
  expect(within(dialog).getByText(/^Remux \/media\/a\.mkv/)).toHaveTextContent("/media/b.mkv: tool failed");
});

it("lists the first failed files and says how many more the log holds", async () => {
  const user = userEvent.setup();
  const listed = [0, 1, 2, 3, 4].map((index) => `${index}.mkv: tool failed`);
  shell({ ...idle, job: job({ status: "Failed", completed: 2, failed: 7, total: 9, error: `internal error: 7 of 9 files failed: ${listed.join("; ")}; and 2 more, listed in the job log` }) });

  await user.click(screen.getByRole("button", { name: "Show the 7 failed file(s) and job log" }));
  const dialog = screen.getByRole("dialog");
  expect(within(dialog).getAllByRole("listitem").map((item) => item.textContent)).toEqual(listed);
  expect(within(dialog).getByText("And 2 more, listed in the job log.")).toBeInTheDocument();
});

it("shows the log of a job that completed, and a single file's error as its failure", async () => {
  const user = userEvent.setup();
  const { unmount } = shell({ ...idle, job: job({ lines: ["Reorder /media/01.mkv"] }) });
  await user.click(screen.getByRole("button", { name: "Show job log" }));
  expect(within(screen.getByRole("dialog")).getByText("Reorder /media/01.mkv")).toBeInTheDocument();
  expect(within(screen.getByRole("dialog")).queryByText("Failed files")).not.toBeInTheDocument();
  unmount();

  shell({ ...idle, job: job({ status: "Failed", completed: 0, failed: 1, total: 1, error: "internal error: tool failed" }) });
  await user.click(screen.getByRole("button", { name: "Show the 1 failed file(s) and job log" }));
  expect(within(screen.getByRole("dialog")).getByRole("listitem")).toHaveTextContent("internal error: tool failed");
  expect(within(screen.getByRole("dialog")).getByText("The job wrote no log lines.")).toBeInTheDocument();
});
