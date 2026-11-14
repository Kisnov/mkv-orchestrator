import { describe, expect, it, vi } from "vitest";
import type { MediaFileRow } from "../api";
import { HttpBackendClient } from "./httpClient";

describe("HttpBackendClient library audits", () => {
  it("sends compact paths instead of duplicating full scan metadata", async () => {
    const fetchMock = vi.fn().mockResolvedValue(new Response(JSON.stringify({
      summary: { groups: 0, files: 0, issueGroups: 0, standardGroups: 0 },
      items: []
    }), { status: 200, headers: { "Content-Type": "application/json" } }));
    const client = new HttpBackendClient({ fetch: fetchMock });
    const files = Array.from({ length: 9_000 }, (_, index) => ({
      path: `/media/show/episode-${index}.mkv`,
      // If the transport regresses to full rows, this makes the request far
      // larger than the server's default 16 MiB body limit.
      tracks: [{ title: "x".repeat(2_000) }]
    })) as unknown as MediaFileRow[];

    await client.buildLibraryAudit(files);

    const [, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    const body = JSON.parse(String(init.body));
    expect(body).toEqual({ paths: files.map((file) => file.path) });
    expect(String(init.body).length).toBeLessThan(1024 * 1024);
  });
});

describe("HttpBackendClient library posters", () => {
  it("addresses a media-server poster by a versioned image URL", () => {
    const client = new HttpBackendClient({ baseUrl: "http://nas:8080/" });

    expect(client.libraryArtworkUrl({ serverId: "server 1", itemId: "show/1" }, 3))
      .toBe("http://nas:8080/api/library/artwork/server%201/show%2F1?v=3");
  });
});

describe("HttpBackendClient dashboard reads", () => {
  const json = (body: unknown) => new Response(JSON.stringify(body), { status: 200, headers: { "Content-Type": "application/json" } });

  it("lists recent jobs with the limit in the query", async () => {
    const job = { id: "job-1", kind: "scan", status: "Completed", createdUtc: "2026-11-14T10:00:00Z", completed: 3, failed: 0, skipped: 0, total: 3, currentFile: "", currentFilePercent: 0, summary: "Scan completed" };
    const fetchMock = vi.fn().mockResolvedValue(json({ jobs: [job] }));
    const client = new HttpBackendClient({ fetch: fetchMock });

    await expect(client.listRecentJobs(20)).resolves.toEqual({ jobs: [job] });
    expect(fetchMock.mock.calls[0][0]).toBe("/api/jobs/recent?limit=20");
  });

  it("reads watch health and refuses an answer that is not one", async () => {
    const health = { running: true, backend: "polling", watchedRoots: 2, lastEventUtc: "2026-11-14T10:00:00Z" };
    const fetchMock = vi.fn().mockResolvedValueOnce(json(health)).mockResolvedValueOnce(json({ running: "yes" }));
    const client = new HttpBackendClient({ fetch: fetchMock });

    await expect(client.getWatchHealth()).resolves.toEqual(health);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/watch/health");
    await expect(client.getWatchHealth()).rejects.toMatchObject({ code: "INVALID_RESPONSE" });
  });
});

describe("HttpBackendClient timeouts", () => {
  function timedOutSignal() {
    const controller = new AbortController();
    controller.abort(new DOMException("signal timed out", "TimeoutError"));
    return controller.signal;
  }

  it("reports a request the server never answers as a timeout", async () => {
    const timeout = vi.spyOn(AbortSignal, "timeout").mockReturnValue(timedOutSignal());
    const fetchMock = vi.fn((_input: RequestInfo | URL, init?: RequestInit) => Promise.reject(init?.signal?.reason));
    const client = new HttpBackendClient({ fetch: fetchMock });

    await expect(client.getStatus()).rejects.toMatchObject({ name: "ApiError", code: "TIMEOUT" });
    expect(timeout).toHaveBeenCalledWith(60_000);
  });

  it("reports a body cut off by the timeout as a timeout in browsers that call it AbortError", async () => {
    vi.spyOn(AbortSignal, "timeout").mockReturnValue(timedOutSignal());
    const response = { text: () => Promise.reject(new DOMException("aborted", "AbortError")) } as Response;
    const client = new HttpBackendClient({ fetch: vi.fn().mockResolvedValue(response) });

    await expect(client.getStatus()).rejects.toMatchObject({ name: "ApiError", code: "TIMEOUT" });
  });

  it("gives calls that do their work inline a much longer limit", async () => {
    const timeout = vi.spyOn(AbortSignal, "timeout").mockReturnValue(timedOutSignal());
    const client = new HttpBackendClient({ fetch: vi.fn().mockRejectedValue(new Error("offline")) });

    await expect(client.applyRenamePreview({ previewId: "p" } as never)).rejects.toThrow();
    await expect(client.loadPropEditTemplate({ path: "/media/a.mkv" } as never)).rejects.toThrow();
    await expect(client.getLibraryCatalog({} as never)).rejects.toThrow();

    expect(timeout.mock.calls).toEqual([[30 * 60_000], [30 * 60_000], [30 * 60_000]]);
  });
});

describe("HttpBackendClient job polling", () => {
  function pollWith(status: number) {
    const fetchMock = vi.fn(() => Promise.resolve(new Response('{"detail":"nope"}', { status })));
    const client = new HttpBackendClient({ fetch: fetchMock });
    const onError = vi.fn();
    return { fetchMock, onError, start: () => client.subscribeJobProgress({ jobId: "j", kind: "operation", pollIntervalMs: 250 }, vi.fn(), onError) };
  }

  it("stops polling once the session is rejected", async () => {
    vi.useFakeTimers();
    try {
      const { fetchMock, onError, start } = pollWith(401);
      await start();
      await vi.advanceTimersByTimeAsync(2_000);

      expect(fetchMock).toHaveBeenCalledTimes(1);
      expect(onError).toHaveBeenCalledWith(expect.objectContaining({ status: 401 }));
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps polling through other failures", async () => {
    vi.useFakeTimers();
    try {
      const { fetchMock, start } = pollWith(503);
      const stop = await start();
      await vi.advanceTimersByTimeAsync(2_000);
      stop();

      expect(fetchMock.mock.calls.length).toBeGreaterThan(1);
    } finally {
      vi.useRealTimers();
    }
  });
});
