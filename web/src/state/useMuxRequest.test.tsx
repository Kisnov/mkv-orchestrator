import type { ReactNode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { BackendClient } from "../backend/client";
import { createMockBackendClient } from "../backend/mockClient";
import { setBackendClient } from "../backend/runtime";
import type { MuxPreviewRequest, MuxPreviewResponse, OperationJobResponse } from "../api";
import { OperationJobProvider } from "./OperationJobContext";
import { buildMuxRequest, useMuxRequest } from "./useMuxRequest";

const plan: MuxPreviewResponse = {
  actions: [{ filePath: "/media/01.mkv", fileName: "01.mkv", index: 0, operation: "Remux", toolName: "mkvmerge", description: "Remux", command: "mkvmerge" }],
  noChangeFiles: [], summary: "1 file", status: "Ready to apply", planId: "plan", planFingerprint: "fingerprint", idempotencyKey: null
};

function job(patch: Partial<OperationJobResponse>): OperationJobResponse {
  return {
    id: "job-1", kind: "Remux", status: "Running", createdUtc: "", startedUtc: null, completedUtc: null,
    completed: 0, failed: 0, skipped: 0, total: 2, currentFile: "", currentFilePercent: 0, lines: [], muxResult: null, propEditResult: null, error: "",
    ...patch
  };
}

function setup(overrides: Partial<BackendClient>, options: { reusePreview?: boolean; legacyLabel?: string } = {}) {
  setBackendClient(createMockBackendClient(overrides));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 }, mutations: { retry: false } } });
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={client}><OperationJobProvider>{children}</OperationJobProvider></QueryClientProvider>;
  return renderHook(() => useMuxRequest({ label: "Reorder Tracks", initialStatus: "Pick files.", startedText: (total) => `Reordering ${total} file(s)...`, noChangesText: "Nothing to change.", ...options }), { wrapper });
}

const request = buildMuxRequest([], ["/media/01.mkv"], { reorderTracks: true });

beforeEach(() => sessionStorage.clear());

describe("buildMuxRequest", () => {
  it("turns every step off except the ones a page asks for", () => {
    expect(request).toMatchObject({ selectedPaths: ["/media/01.mkv"], reorderTracks: true, formatSubtitles: false, convertMp4ToMkv: false, preserveChapters: true, subtitleFontSize: "48", externalSubtitleTrackName: null });
  });
});

describe("useMuxRequest", () => {
  it("plans, applies that plan once with an idempotency key, and tracks the job", async () => {
    const startMuxApply = vi.fn((sent: MuxPreviewRequest) => Promise.resolve(job({ total: sent.selectedPaths.length })));
    const { result } = setup({ buildMuxPreview: () => Promise.resolve(plan), startMuxApply, getOperationJob: () => Promise.resolve(job({ total: 1 })) });

    await act(() => result.current.apply(request));

    expect(startMuxApply).toHaveBeenCalledTimes(1);
    const sent = startMuxApply.mock.calls[0][0];
    expect(sent).toMatchObject({ planId: "plan", planFingerprint: "fingerprint", reorderTracks: true });
    expect(sent.idempotencyKey).toMatch(/^[0-9a-f-]{36}$/);
    await waitFor(() => expect(result.current.isApplying).toBe(true));
    expect(JSON.parse(sessionStorage.getItem("mkvo.web.activeOperationJob")!)).toEqual([{ id: "job-1", label: "Reorder Tracks" }]);
  });

  it("says so and applies nothing when the plan has no actions", async () => {
    const startMuxApply = vi.fn();
    const { result } = setup({ buildMuxPreview: () => Promise.resolve({ ...plan, actions: [] }), startMuxApply });

    await act(() => result.current.apply(request));

    expect(startMuxApply).not.toHaveBeenCalled();
    expect(result.current.status).toBe("Nothing to change.");
  });

  it("holds the plan when confirmation is refused, and applies it when asked", async () => {
    const startMuxApply = vi.fn(() => Promise.resolve(job({})));
    const { result } = setup({ buildMuxPreview: () => Promise.resolve(plan), startMuxApply, getOperationJob: () => Promise.resolve(job({})) });

    let held: MuxPreviewResponse | null = null;
    await act(() => result.current.apply(request, (planned) => { held = planned; return false; }));
    expect(startMuxApply).not.toHaveBeenCalled();

    await act(() => result.current.applyPlan(request, held!));
    expect(startMuxApply).toHaveBeenCalledWith(expect.objectContaining({ planId: "plan" }));
  });

  it("reuses the previewed plan only when asked to and the request is unchanged", async () => {
    const buildMuxPreview = vi.fn(() => Promise.resolve(plan));
    const { result } = setup({ buildMuxPreview, startMuxApply: () => Promise.resolve(job({})), getOperationJob: () => Promise.resolve(job({})) }, { reusePreview: true });

    act(() => result.current.preview(request));
    await waitFor(() => expect(result.current.previewResult).toEqual(plan));
    await act(() => result.current.apply(request));
    expect(buildMuxPreview).toHaveBeenCalledTimes(1);
  });

  it("reports progress, then the failure the finished job carries", async () => {
    let current = job({ completed: 1, currentFile: "02.mkv", currentFilePercent: 40 });
    const { result } = setup({ buildMuxPreview: () => Promise.resolve(plan), startMuxApply: () => Promise.resolve(current), getOperationJob: () => Promise.resolve(current) });

    await act(() => result.current.apply(request));
    await waitFor(() => expect(result.current.status).toBe("Applying 1/2 | 02.mkv 40%"));

    current = job({ status: "Failed", completed: 1, failed: 1, error: "internal error: 1 of 2 files failed: 02.mkv: tool failed" });
    await waitFor(() => expect(result.current.status).toBe("internal error: 1 of 2 files failed: 02.mkv: tool failed"), { timeout: 3000 });
    expect(result.current.job?.status).toBe("Failed");
    expect(result.current.isApplying).toBe(false);
  });

  it("shows the plan a finished job ran", async () => {
    const ran = { ...plan, status: "Reordered 1 file" };
    const finished = job({ status: "Completed", completed: 1, total: 1, muxResult: ran });
    sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify({ id: "job-1", label: "Reorder Tracks" }));
    const { result } = setup({ getOperationJob: () => Promise.resolve(finished) });

    await waitFor(() => expect(result.current.previewResult).toEqual(ran));
    expect(result.current.status).toBe("Reordered 1 file");
  });

  it("keeps its own job while another page's newer job runs", async () => {
    sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify([{ id: "job-2", label: "Edit Tracks" }, { id: "job-1", label: "Reorder Tracks" }]));
    const { result } = setup({ getOperationJob: (id) => Promise.resolve(job({ id })) });

    await waitFor(() => expect(result.current.job?.id).toBe("job-1"));
    expect(result.current.isApplying).toBe(true);
  });

  it("adopts a job stored under the name the page used before", async () => {
    sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify({ id: "job-1", label: "MKV Operations" }));
    const { result } = setup({ getOperationJob: () => Promise.resolve(job({})) }, { legacyLabel: "MKV Operations" });

    await waitFor(() => expect(result.current.job?.id).toBe("job-1"));
    expect(result.current.isApplying).toBe(true);
  });

  it("ignores another page's job", async () => {
    sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify({ id: "job-1", label: "Edit Tracks" }));
    const { result } = setup({ getOperationJob: () => Promise.resolve(job({})) });

    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(result.current.job).toBeUndefined();
    expect(result.current.isApplying).toBe(false);
    expect(result.current.status).toBe("Pick files.");
  });
});
