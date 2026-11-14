import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, expect, it, vi } from "vitest";

import type { OperationJobResponse } from "../api";
import { ApiError } from "../backend/error";
import { createMockBackendClient } from "../backend/mockClient";
import { setBackendClient } from "../backend/runtime";
import { OperationJobProvider, useOperationJob } from "./OperationJobContext";

const storageKey = "mkvo.web.activeOperationJob";

function job(id: string, patch: Partial<OperationJobResponse> = {}): OperationJobResponse {
  return {
    id, kind: "Remux", status: "Running", createdUtc: "", startedUtc: null, completedUtc: null,
    completed: 1, failed: 0, skipped: 0, total: 4, currentFile: "", currentFilePercent: 0, lines: [], muxResult: null, propEditResult: null, error: "",
    ...patch
  };
}

function setup(getOperationJob: (id: string) => Promise<OperationJobResponse>) {
  setBackendClient(createMockBackendClient({ getOperationJob }));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}><OperationJobProvider>{children}</OperationJobProvider></QueryClientProvider>;
  return renderHook(() => useOperationJob(), { wrapper });
}

beforeEach(() => sessionStorage.clear());

it("keeps following a job when another page starts one", async () => {
  const { result } = setup((id) => Promise.resolve(job(id)));

  act(() => result.current.trackJob("edit-job", "Edit Tracks"));
  act(() => result.current.trackJob("reorder-job", "Reorder Tracks"));

  await waitFor(() => expect(result.current.operationFor("Edit Tracks")?.job?.id).toBe("edit-job"));
  await waitFor(() => expect(result.current.operationFor("Reorder Tracks")?.job?.id).toBe("reorder-job"));
  expect(result.current.operations.map((operation) => operation.statusText)).toEqual(["Reorder Tracks: 1/4", "Edit Tracks: 1/4"]);
  expect(result.current.operations.every((operation) => operation.isRunning)).toBe(true);
  // The newest job is still what the single-job fields describe.
  expect(result.current.activeOperation).toEqual({ id: "reorder-job", label: "Reorder Tracks" });
  expect(JSON.parse(sessionStorage.getItem(storageKey)!)).toEqual([
    { id: "reorder-job", label: "Reorder Tracks" },
    { id: "edit-job", label: "Edit Tracks" }
  ]);
});

it("replaces a page's earlier job with its next one", () => {
  const { result } = setup((id) => Promise.resolve(job(id)));

  act(() => result.current.trackJob("first", "Edit Tracks"));
  act(() => result.current.trackJob("second", "Edit Tracks"));

  expect(result.current.operations.map((operation) => operation.id)).toEqual(["second"]);
});

it("restores the jobs a reload remembers, in either stored shape", async () => {
  sessionStorage.setItem(storageKey, JSON.stringify({ id: "old-shape", label: "Edit Tracks" }));
  const single = setup((id) => Promise.resolve(job(id)));
  await waitFor(() => expect(single.result.current.operationFor("Edit Tracks")?.job?.id).toBe("old-shape"));
  single.unmount();

  sessionStorage.setItem(storageKey, JSON.stringify([{ id: "a", label: "Edit Tracks" }, { id: "b", label: "Subtitle Format" }]));
  const many = setup((id) => Promise.resolve(job(id)));
  await waitFor(() => expect(many.result.current.operations.map((operation) => operation.job?.id)).toEqual(["a", "b"]));
});

it("forgets only the job the server no longer knows", async () => {
  sessionStorage.setItem(storageKey, JSON.stringify([{ id: "gone", label: "Edit Tracks" }, { id: "live", label: "Subtitle Format" }]));
  const getOperationJob = vi.fn((id: string) => id === "gone"
    ? Promise.reject(new ApiError("Job not found", { code: "not_found", status: 404 }))
    : Promise.resolve(job(id, { status: "Completed", completed: 4 })));
  const { result } = setup(getOperationJob);

  await waitFor(() => expect(result.current.operations.map((operation) => operation.id)).toEqual(["live"]));
  await waitFor(() => expect(JSON.parse(sessionStorage.getItem(storageKey)!)).toEqual([{ id: "live", label: "Subtitle Format" }]));
  expect(result.current.statusText).toBe("Subtitle Format: completed 4/4");
  expect(result.current.isRunning).toBe(false);
});

function setupWithFiles(getOperationJob: (id: string) => Promise<OperationJobResponse>) {
  const readFiles = vi.fn(() => Promise.resolve("files"));
  setBackendClient(createMockBackendClient({ getOperationJob }));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}><OperationJobProvider>{children}</OperationJobProvider></QueryClientProvider>;
  const hook = renderHook(() => {
    useQuery({ queryKey: ["current-scan-files"], queryFn: readFiles });
    return useOperationJob();
  }, { wrapper });
  return { ...hook, readFiles };
}

it("does not read the scanned files again for a job that was over when the tab loaded", async () => {
  sessionStorage.setItem(storageKey, JSON.stringify([{ id: "done", label: "Edit Tracks" }]));
  const { result, readFiles } = setupWithFiles((id) => Promise.resolve(job(id, { status: "Completed", completed: 4 })));

  await waitFor(() => expect(result.current.statusText).toBe("Edit Tracks: completed 4/4"));
  await new Promise((resolve) => setTimeout(resolve, 50));
  expect(readFiles).toHaveBeenCalledTimes(1);
});

it("reads the scanned files once when a job it follows ends", async () => {
  let status: OperationJobResponse["status"] = "Running";
  const { result, readFiles } = setupWithFiles((id) => Promise.resolve(job(id, { status })));

  act(() => result.current.trackJob("live", "Edit Tracks"));
  await waitFor(() => expect(result.current.job?.status).toBe("Running"));
  status = "Completed";
  await waitFor(() => expect(result.current.job?.status).toBe("Completed"), { timeout: 3000 });
  await waitFor(() => expect(readFiles).toHaveBeenCalledTimes(2));
});

it("stops remembering a job that ended more than a day ago", async () => {
  sessionStorage.setItem(storageKey, JSON.stringify([{ id: "old", label: "Edit Tracks" }, { id: "recent", label: "Subtitle Format" }]));
  const ended = (hoursAgo: number) => new Date(Date.now() - hoursAgo * 3_600_000).toISOString();
  setup((id) => Promise.resolve(job(id, { status: "Completed", completedUtc: ended(id === "old" ? 30 : 2) })));

  await waitFor(() => expect(JSON.parse(sessionStorage.getItem(storageKey)!)).toEqual([{ id: "recent", label: "Subtitle Format" }]));
});
