import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, useLocation } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";

import App from "../App";
import type { BackendClient } from "../backend/client";
import { ApiError } from "../backend/error";
import { createMockBackendClient } from "../backend/mockClient";
import { setBackendClient } from "../backend/runtime";
import type { MediaFileRow, OperationJobResponse, RecentJob, WebSettings } from "../generated/contracts";
import { MediaLibraryProvider, MediaLibrarySynchronizer } from "../state/MediaLibraryContext";
import { renderWithBackend } from "../test/render";
import { DashboardPage } from "./DashboardPage";

const tool = (name: string, patch = {}) => ({ name, command: name, resolvedPath: `/usr/bin/${name}`, available: true, version: `${name} v102.0 ('Little Houses') 64-bit`, ...patch });
const status = { name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [tool("mkvmerge"), tool("mkvpropedit")], contractVersion: 1 };
const settings = { enableLiveWatchFolderMonitoring: false, watchFolders: [], mediaServers: [] } as unknown as WebSettings;
const watchOff = { running: false, backend: "native" as const, watchedRoots: 0 };
const minutesAgo = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

function recentJob(id: string, patch: Partial<RecentJob> = {}): RecentJob {
  return { id, kind: "remux", status: "Completed", createdUtc: minutesAgo(6), startedUtc: minutesAgo(6), completedUtc: minutesAgo(5), completed: 4, failed: 0, skipped: 0, total: 4, currentFile: "", currentFilePercent: 0, summary: "Mux/remux operation completed", ...patch };
}

function mediaFile(name: string, patch: Partial<MediaFileRow> = {}): MediaFileRow {
  return {
    path: `/media/Show/${name}`, fileName: name, extension: ".mkv", status: "Scanned", reader: "mkvmerge", codec: "HEVC/H.265", resolution: "1920x1080", bitDepth: "10", hdr: "None",
    videoSummary: "", audioSummary: "jpn x1", subtitleSummary: "eng x1", attachmentSummary: "None", attachments: [],
    tracks: [{ id: 1, trackNumber: 2, type: "audio", codec: "AAC", language: "jpn", name: "", channels: 2, default: true, forced: false }],
    ...patch
  };
}

function backend(overrides: Partial<BackendClient> = {}): Partial<BackendClient> {
  return {
    getStatus: () => Promise.resolve(status),
    getWebSettings: () => Promise.resolve(settings),
    listRecentJobs: () => Promise.resolve({ jobs: [] }),
    getWatchHealth: () => Promise.resolve(watchOff),
    getRenameBatches: () => Promise.resolve({ batches: [] }),
    getCurrentScanFiles: () => Promise.resolve({ updatedUtc: null, files: [], selectedPaths: [], summary: { total: 0, mkv: 0, mp4: 0, failed: 0, cached: 0 } }),
    ...overrides
  };
}

function renderDashboard(overrides: Partial<BackendClient> = {}) {
  return renderWithBackend(<MediaLibraryProvider><MediaLibrarySynchronizer /><DashboardPage /></MediaLibraryProvider>, backend(overrides));
}

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
});

describe("Dashboard on a fresh install", () => {
  it("says nothing needs attention and points at Scan", async () => {
    renderDashboard();

    expect(await screen.findByText("Nothing needs your attention.")).toBeInTheDocument();
    expect(screen.getByText("No jobs yet")).toBeInTheDocument();
    expect(screen.getByText("No files scanned yet")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Scan a folder" })).toHaveAttribute("href", "/dashboard/scan");
    // Tool versions read as numbers, with the full banner one hover away.
    const system = within(screen.getByRole("region", { name: "System" }));
    expect(await system.findByText("mkvmerge")).toBeInTheDocument();
    expect(system.getAllByText("102.0")).toHaveLength(2);
    expect(system.getByText("Off")).toBeInTheDocument();
    expect(system.getByText("None configured")).toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Recent renames" })).not.toBeInTheDocument();
  });
});

describe("Dashboard activity", () => {
  it("lists running jobs first with progress, under the name of the page that started them", async () => {
    sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify([{ id: "reorder", label: "Reorder Tracks" }, { id: "extract", label: "MKV Operations" }]));
    renderDashboard({
      getOperationJob: () => new Promise(() => undefined),
      listRecentJobs: () => Promise.resolve({ jobs: [
        recentJob("old"),
        recentJob("extract", { kind: "extract_subtitles" }),
        recentJob("reorder", { status: "Running", completedUtc: null, completed: 1, total: 4, currentFile: "/media/Show/02.mkv", currentFilePercent: 50 }),
        recentJob("scan", { kind: "scan", status: "Running", completedUtc: null, completed: 12, total: 128, currentFile: "/media/Show/13.mkv" })
      ] })
    });

    const rows = await within(await screen.findByRole("list", { name: "Running and recent jobs" })).findAllByRole("listitem");
    // A remux takes its page's name; a kind that names itself keeps it.
    expect(rows.map((row) => row.querySelector(".font-semibold")?.textContent)).toEqual(["Reorder Tracks", "Scan", "Remux", "Extract Subtitles"]);
    expect(within(rows[0]).getByText("1 of 4 files")).toBeInTheDocument();
    expect(within(rows[0]).getByText("02.mkv 50%")).toBeInTheDocument();
    expect(within(rows[0]).getByRole("progressbar", { name: "Reorder Tracks progress" })).toHaveAttribute("aria-valuenow", "38");
    expect(within(rows[1]).getByText("13.mkv")).toBeInTheDocument();
    expect(within(rows[2]).getByText("4 done")).toBeInTheDocument();
    expect(screen.getByText("2 running")).toBeInTheDocument();
  });

  it("cancels a scan and an operation through their own endpoints", async () => {
    const user = userEvent.setup();
    const cancelScan = vi.fn(() => new Promise<never>(() => undefined));
    const cancelOperationJob = vi.fn(() => new Promise<never>(() => undefined));
    renderDashboard({
      cancelScan, cancelOperationJob,
      listRecentJobs: () => Promise.resolve({ jobs: [
        recentJob("op", { status: "Running", completedUtc: null, completed: 1 }),
        recentJob("scan", { kind: "scan", status: "Queued", completedUtc: null, completed: 0, total: 0 })
      ] })
    });

    await user.click(await screen.findByRole("button", { name: "Cancel Scan" }));
    await user.click(screen.getByRole("button", { name: "Cancel Remux" }));
    expect(cancelScan).toHaveBeenCalledWith("scan");
    expect(cancelOperationJob).toHaveBeenCalledWith("op");
  });

  it("opens a finished job's log on demand, and offers none for scans", async () => {
    const user = userEvent.setup();
    const job: OperationJobResponse = {
      id: "failed", kind: "mux", status: "Failed", createdUtc: minutesAgo(6), startedUtc: minutesAgo(6), completedUtc: minutesAgo(5), completed: 3, failed: 1, skipped: 0, total: 4,
      currentFile: "", currentFilePercent: 0, lines: ["Remux 01.mkv", "Failed 04.mkv"], muxResult: null, propEditResult: null, error: "internal error: 1 of 4 files failed: 04.mkv: tool failed"
    };
    const getOperationJob = vi.fn(() => Promise.resolve(job));
    renderDashboard({
      getOperationJob,
      listRecentJobs: () => Promise.resolve({ jobs: [
        recentJob("failed", { status: "Failed", completed: 3, failed: 1, error: job.error }),
        recentJob("scan", { kind: "scan", summary: "Scan completed" })
      ] })
    });

    const rows = await within(await screen.findByRole("list", { name: "Running and recent jobs" })).findAllByRole("listitem");
    expect(within(rows[0]).getByText("1 of 4 failed")).toBeInTheDocument();
    expect(within(rows[0]).getByText("1 of 4 files failed: 04.mkv: tool failed")).toBeInTheDocument();
    // Nothing says which page ran this remux, so there is nowhere to send a retry.
    expect(within(rows[0]).queryByRole("link")).not.toBeInTheDocument();
    expect(within(rows[1]).queryByRole("button")).not.toBeInTheDocument();
    expect(getOperationJob).not.toHaveBeenCalled();

    await user.click(within(rows[0]).getByRole("button", { name: /View log of Remux/ }));
    const dialog = await screen.findByRole("dialog", { name: "Remux log" });
    expect(within(dialog).getByText("04.mkv: tool failed")).toBeInTheDocument();
    expect(getOperationJob).toHaveBeenCalledTimes(1);
  });

  it("sends a retry to the page that ran the job, in this tab or an earlier one", async () => {
    localStorage.setItem("mkvo.web.jobLabels", JSON.stringify([["reorder", "Reorder Tracks"]]));
    renderDashboard({ listRecentJobs: () => Promise.resolve({ jobs: [
      recentJob("reorder", { status: "Failed", failed: 1, error: "tool failed" }),
      recentJob("edit", { kind: "property_edit", status: "Failed", failed: 2, error: "tool failed" })
    ] }) });

    const rows = await within(await screen.findByRole("list", { name: "Running and recent jobs" })).findAllByRole("listitem");
    expect(within(rows[0]).getByRole("link", { name: /^Retry in Reorder Tracks/ })).toHaveAttribute("href", "/tracks/reorder");
    expect(within(rows[1]).getByRole("link", { name: /^Retry in Edit Tracks/ })).toHaveAttribute("href", "/tracks/properties");
  });

  it("says why the list is empty when it cannot be read, and tries again", async () => {
    const user = userEvent.setup();
    const listRecentJobs = vi.fn<() => Promise<{ jobs: RecentJob[] }>>().mockRejectedValueOnce(new ApiError("database is locked", { code: "internal", status: 500 })).mockResolvedValue({ jobs: [recentJob("ok")] });
    renderDashboard({ listRecentJobs });

    expect(await screen.findByText("Jobs could not be loaded: database is locked")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("4 done")).toBeInTheDocument();
  });
});

describe("Dashboard attention list", () => {
  it("names each problem once with the place to fix it", async () => {
    const testMediaServerConnection = vi.fn(() => Promise.resolve({ success: false, status: "Jellyfin rejected the stored API key (HTTP 401). Enter the key again in Settings.", libraryCount: 0 }));
    renderDashboard({
      getStatus: () => Promise.resolve({ ...status, tools: [tool("mkvmerge"), tool("mkvpropedit", { available: false, version: "", error: "mkvpropedit was not found" })] }),
      getWebSettings: () => Promise.resolve({ ...settings, enableLiveWatchFolderMonitoring: true, mediaServers: [{ id: "server-1", name: "Jellyfin", type: "Jellyfin", serverUrl: "http://nas:8096", hasApiKey: true, isDefault: true, lastSyncedUtc: null, libraries: [] }] }),
      getWatchHealth: () => Promise.resolve({ ...watchOff, error: "The watch folder does not exist." }),
      testMediaServerConnection,
      listRecentJobs: () => Promise.resolve({ jobs: [recentJob("scan", { kind: "scan" }), recentJob("failed", { status: "Failed", failed: 1, error: "tool failed" })] })
    });

    const attention = within(screen.getByRole("region", { name: "Needs attention" }));
    expect(await attention.findByText("mkvpropedit is not available: mkvpropedit was not found.")).toBeInTheDocument();
    expect(attention.getByRole("link", { name: "Tool settings" })).toHaveAttribute("href", "/settings?tab=general");
    expect(await attention.findByText("Watch folders: The watch folder does not exist.")).toBeInTheDocument();
    expect(await attention.findByText("Jellyfin rejected the stored API key (HTTP 401). Enter the key again in Settings.")).toBeInTheDocument();
    expect(attention.getByRole("link", { name: "Server settings" })).toHaveAttribute("href", "/settings?tab=library");
    expect(await attention.findByText("Remux failed 5 minutes ago.")).toBeInTheDocument();
    expect(attention.getAllByRole("listitem")).toHaveLength(4);
    expect(testMediaServerConnection).toHaveBeenCalledTimes(1);
    expect(testMediaServerConnection).toHaveBeenCalledWith({ id: "server-1" });
  });

  it("checks a media server once and again only when asked", async () => {
    const user = userEvent.setup();
    const testMediaServerConnection = vi.fn(() => Promise.resolve({ success: true, status: "Connection succeeded.", libraryCount: 3 }));
    const servers = [
      { id: "server-1", name: "Jellyfin", type: "Jellyfin", serverUrl: "http://nas:8096", hasApiKey: true, isDefault: true, lastSyncedUtc: null, libraries: [] },
      { id: "server-2", name: "Plex", type: "Plex", serverUrl: "http://nas:32400", hasApiKey: false, isDefault: false, lastSyncedUtc: null, libraries: [] }
    ];
    const { unmount } = renderDashboard({ getWebSettings: () => Promise.resolve({ ...settings, mediaServers: servers }), testMediaServerConnection });

    expect(await screen.findByText("Connected, 3 library paths")).toBeInTheDocument();
    // A server without a key is not asked anything.
    expect(screen.getByText("No API key is saved.")).toBeInTheDocument();
    expect(testMediaServerConnection).toHaveBeenCalledTimes(1);

    await user.click(screen.getByRole("button", { name: "Check Jellyfin again" }));
    await waitFor(() => expect(testMediaServerConnection).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole("button", { name: "Check Plex again" })).not.toBeInTheDocument();
    unmount();
  });
});

describe("Dashboard scanned files", () => {
  it("summarises the files the app already holds without reading the list again", async () => {
    const files = [mediaFile("01.mkv"), mediaFile("02.mkv"), mediaFile("03.mkv", { codec: "AVC/H.264", audioSummary: "eng x1" }), mediaFile("04.mp4", { extension: ".mp4", status: "Scan failed" })];
    const getCurrentScanFiles = vi.fn(() => Promise.resolve({ updatedUtc: "2026-11-14T10:00:00Z", files, selectedPaths: [], summary: { total: 4, mkv: 3, mp4: 1, failed: 1, cached: 0 } }));
    sessionStorage.setItem("mkvo.web.scanSources", JSON.stringify(["/media/Show/"]));
    renderDashboard({ getCurrentScanFiles });

    const card = within(screen.getByRole("region", { name: "Scanned files" }));
    expect(await card.findByText("3 MKV")).toBeInTheDocument();
    expect(card.getByText("1 MP4")).toBeInTheDocument();
    expect(card.getByText("1 unreadable")).toBeInTheDocument();
    expect(card.getByText("From Show")).toBeInTheDocument();
    expect(card.getByText("01.mkv")).toBeInTheDocument();
    expect(card.getByText(/1 file differs from it/)).toBeInTheDocument();
    const video = within(card.getByRole("heading", { name: "Video" }).parentElement!);
    expect(video.getAllByRole("listitem").map((item) => item.textContent)).toEqual(["HEVC/H.2652", "AVC/H.2641"]);
    expect(card.getByRole("link", { name: "Open Scan" })).toHaveAttribute("href", "/dashboard/scan");
    expect(within(screen.getByRole("region", { name: "Needs attention" })).getByText("1 scanned file could not be read.")).toBeInTheDocument();
    // The synchronizer's read is the only one.
    expect(getCurrentScanFiles).toHaveBeenCalledTimes(1);
  });

  it("says a scan is running instead of inviting another", async () => {
    renderDashboard({ listRecentJobs: () => Promise.resolve({ jobs: [recentJob("scan", { kind: "scan", status: "Running", completedUtc: null })] }) });

    expect(await screen.findByText("A scan is running")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Scan a folder" })).not.toBeInTheDocument();
  });
});

describe("Dashboard polling", () => {
  it("re-reads only the watcher's state on its own timer", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const getWatchHealth = vi.fn(() => Promise.resolve({ running: true, backend: "polling" as const, watchedRoots: 2, lastEventUtc: minutesAgo(7) }));
    const calls = { getStatus: vi.fn(() => Promise.resolve(status)), getWebSettings: vi.fn(() => Promise.resolve(settings)), getRenameBatches: vi.fn(() => Promise.resolve({ batches: [] })), listRecentJobs: vi.fn(() => Promise.resolve({ jobs: [] })) };
    renderDashboard({ getWatchHealth, ...calls });

    expect(await screen.findByText("Watching 2 folders")).toBeInTheDocument();
    expect(screen.getByText("Polling, last change 7 minutes ago")).toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(31_000));
    expect(getWatchHealth).toHaveBeenCalledTimes(2);
    // The job list is the sidebar's poll; this page only reads it.
    for (const call of Object.values(calls)) expect(call).toHaveBeenCalledTimes(1);
    vi.useRealTimers();
  });
});

describe("Dashboard recent renames", () => {
  it("links a batch to the undo dialog in Rename Files", async () => {
    const entry = { originalPath: "/media/a.mkv", renamedPath: "/media/Show - S01E01.mkv", originalFileName: "a.mkv", renamedFileName: "Show - S01E01.mkv" };
    renderDashboard({ getRenameBatches: () => Promise.resolve({ batches: [
      { id: "batch-1", createdAt: minutesAgo(90), undoneAt: null, provider: "TVDB", template: "", totalFiles: 12, entries: [entry], isUndone: false, displayName: "2026-11-14 09:00 - 12 file(s)" },
      { id: "batch-2", createdAt: minutesAgo(3000), undoneAt: minutesAgo(2000), provider: "TVDB", template: "", totalFiles: 1, entries: [], isUndone: true, displayName: "2026-11-12 09:00 - 1 file(s)" }
    ] }) });

    const card = within(await screen.findByRole("region", { name: "Recent renames" }));
    expect(card.getByText("Show - S01E01.mkv")).toBeInTheDocument();
    expect(card.getByText("12 files, 1 hour ago")).toBeInTheDocument();
    expect(card.getByRole("link", { name: "Undo the rename of Show - S01E01.mkv" })).toHaveAttribute("href", "/rename");
    expect(card.getByText("Undone")).toBeInTheDocument();
    expect(card.getAllByRole("link")).toHaveLength(1);
  });
});

describe("Dashboard routes", () => {
  function Location() { return <output aria-label="Location">{useLocation().pathname}</output>; }

  function renderApp(path: string) {
    setBackendClient(createMockBackendClient({ transport: "tauri", ...backend() }));
    render(
      <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
        <MemoryRouter initialEntries={[path]}><App /><Location /></MemoryRouter>
      </QueryClientProvider>
    );
  }

  it("opens on the overview, with Scan one tab away", async () => {
    const user = userEvent.setup();
    renderApp("/");

    expect(await screen.findByText("Nothing needs your attention.")).toBeInTheDocument();
    expect(screen.getByLabelText("Location")).toHaveTextContent("/dashboard");
    const tabs = within(screen.getByRole("navigation", { name: "Dashboard views" }));
    expect(tabs.getByRole("link", { name: "Overview" })).toHaveAttribute("aria-current", "page");

    await user.click(tabs.getByRole("link", { name: "Scan" }));
    expect(await screen.findByRole("heading", { name: "Scan Sources" })).toBeInTheDocument();
    expect(tabs.getByRole("link", { name: "Scan" })).toHaveAttribute("aria-current", "page");
    expect(tabs.getByRole("link", { name: "Overview" })).not.toHaveAttribute("aria-current");
    // Scan is a view of Dashboard, so the menu still marks Dashboard.
    expect(within(screen.getByRole("complementary")).getByRole("link", { name: "Dashboard" })).toHaveAttribute("aria-current", "page");
  });

  it("sends /scan to the Scan view", async () => {
    renderApp("/scan");

    expect(await screen.findByRole("heading", { name: "Scan Sources" })).toBeInTheDocument();
    expect(screen.getByLabelText("Location")).toHaveTextContent("/dashboard/scan");
  });
});
