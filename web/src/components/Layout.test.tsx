import userEvent from "@testing-library/user-event";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { Route, Routes } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { Layout } from "./Layout";
import { MediaLibraryProvider, MediaLibrarySynchronizer } from "../state/MediaLibraryContext";
import { renderWithBackend } from "../test/render";
import { ApiError } from "../backend/error";

beforeEach(() => {
  window.localStorage.clear();
  window.sessionStorage.clear();
});

describe("global operation status", () => {
  it("shows a running batch while another route is open", async () => {
    window.sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify({ id: "mux-job", label: "MKV Operations" }));
    renderWithBackend(
      <MediaLibraryProvider>
        <Routes>
          <Route element={<Layout />}>
            <Route path="*" element={<div>Track Properties route</div>} />
          </Route>
        </Routes>
      </MediaLibraryProvider>,
      {
        getStatus: () => Promise.resolve({ name: "MKVO", version: "0.1.0", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 }),
        getOperationJob: () => Promise.resolve({
          id: "mux-job", kind: "Remux", status: "Running", createdUtc: "2026-08-25T20:00:00Z", startedUtc: "2026-08-25T20:00:01Z", completedUtc: null,
          completed: 40, failed: 1, skipped: 2, total: 283, currentFile: "Episode 44.mkv", currentFilePercent: 75,
          lines: [], muxResult: null, propEditResult: null, error: ""
        })
      }
    );

    const [operationStatus] = await screen.findAllByText(/MKV Operations: 43\/283.*Episode 44\.mkv 75%/i);
    expect(operationStatus).toBeInTheDocument();
    expect(operationStatus).toHaveClass("break-words", "[overflow-wrap:anywhere]");
    expect(screen.getByText("Track Properties route")).toBeInTheDocument();
  });
});

describe("several jobs at once", () => {
  const status = { name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 };
  const operationJob = (id: string) => ({
    id, kind: "Remux", status: "Running" as const, createdUtc: "2026-11-14T10:00:00Z", startedUtc: null, completedUtc: null,
    completed: 1, failed: 0, skipped: 0, total: 4, currentFile: "", currentFilePercent: 0, lines: [], muxResult: null, propEditResult: null, error: ""
  });
  const recentJob = (id: string, kind: "scan" | "property_edit", completed: number, total: number) => ({
    id, kind, status: "Running" as const, createdUtc: "2026-11-14T10:00:00Z", completed, failed: 0, skipped: 0, total, currentFile: "", currentFilePercent: 0, summary: ""
  });
  const page = <MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Any route</div>} /></Route></Routes></MediaLibraryProvider>;

  it("lists the jobs this tab started and the running ones only the backend knows", async () => {
    window.sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify([{ id: "edit", label: "Edit Tracks" }, { id: "reorder", label: "Reorder Tracks" }]));
    renderWithBackend(page, {
      getStatus: () => Promise.resolve(status),
      getOperationJob: (id) => Promise.resolve(operationJob(id)),
      listRecentJobs: () => Promise.resolve({ jobs: [recentJob("scan-1", "scan", 12, 128), recentJob("edit", "property_edit", 1, 4)] })
    });

    const sidebar = within(screen.getByRole("complementary"));
    expect(await sidebar.findByText("Edit Tracks: 1/4")).toBeInTheDocument();
    expect(sidebar.getByText("Reorder Tracks: 1/4")).toBeInTheDocument();
    expect(await sidebar.findByText("Scan: 12/128")).toBeInTheDocument();
    // The backend lists the tracked job too; it is the same job.
    expect(sidebar.getAllByText(/Edit Tracks: /)).toHaveLength(1);
  });

  it("marks Dashboard in the folded sidebar while a job runs", async () => {
    window.localStorage.setItem("mkvo.sidebar.collapsed", "true");
    renderWithBackend(page, { getStatus: () => Promise.resolve(status), listRecentJobs: () => Promise.resolve({ jobs: [recentJob("scan-1", "scan", 12, 128)] }) });

    await waitFor(() => expect(screen.getByRole("link", { name: "Dashboard" }).querySelector("[data-running]")).not.toBeNull());
  });

  it("re-reads the scanned files when a job only the backend knows has ended", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let jobs = [recentJob("scan-1", "scan", 12, 128)];
    const getCurrentScanFiles = vi.fn(() => Promise.resolve({ updatedUtc: null, files: [], selectedPaths: [], summary: { total: 0, mkv: 0, mp4: 0, failed: 0, cached: 0 } }));
    renderWithBackend(<MediaLibraryProvider><MediaLibrarySynchronizer />{page.props.children}</MediaLibraryProvider>, { getStatus: () => Promise.resolve(status), listRecentJobs: () => Promise.resolve({ jobs }), getCurrentScanFiles });

    expect(await screen.findAllByText("Scan: 12/128")).not.toHaveLength(0);
    expect(getCurrentScanFiles).toHaveBeenCalledTimes(1);
    jobs = [{ ...jobs[0], status: "Completed" as never }];
    await act(() => vi.advanceTimersByTimeAsync(2100));
    await waitFor(() => expect(getCurrentScanFiles).toHaveBeenCalledTimes(2));
    vi.useRealTimers();
  });

  it("stays ready when the job list cannot be read", async () => {
    renderWithBackend(page, { getStatus: () => Promise.resolve(status), listRecentJobs: () => Promise.reject(new ApiError("down", { code: "internal", status: 500 })) });

    expect(await screen.findByText("ready")).toBeInTheDocument();
  });
});


it("navigates from the mobile menu and closes it after changing routes", async () => {
  const user = userEvent.setup();
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Dashboard content</div>} /><Route path="/subtitles" element={<div>Subtitle content</div>} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 })
  });
  await user.click(screen.getByRole("button", { name: "Menu" }));
  const menu = screen.getByRole("navigation", { name: "Mobile navigation" });
  await user.click(within(menu).getByRole("link", { name: "Subtitles" }));
  expect(await screen.findByText("Subtitle content")).toBeInTheDocument();
  expect(screen.queryByRole("navigation", { name: "Mobile navigation" })).not.toBeInTheDocument();

});

it("shows a styled tooltip for collapsed sidebar links on hover and focus", async () => {
  window.localStorage.setItem("mkvo.sidebar.collapsed", "true");
  const user = userEvent.setup();
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Dashboard content</div>} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 })
  });

  const tracks = screen.getByRole("link", { name: "Tracks" });
  expect(tracks).not.toHaveAttribute("title");
  await user.hover(tracks);
  expect(screen.getByRole("tooltip")).toHaveTextContent("Tracks");
  await user.unhover(tracks);
  expect(screen.queryByRole("tooltip")).not.toBeInTheDocument();
  fireEvent.focus(tracks);
  expect(screen.getByRole("tooltip")).toHaveTextContent("Tracks");
});

it("lists seven destinations, with the track operations under Tracks", () => {
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Dashboard content</div>} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 })
  });

  const menu = screen.getByRole("complementary");
  expect(within(menu).getAllByRole("link").map((link) => link.textContent)).toEqual(["Dashboard", "Rename Files", "Tracks", "Subtitles", "Library", "Logs", "Settings"]);
  expect(within(menu).getByRole("link", { name: "Tracks" })).toHaveAttribute("href", "/tracks");
});

it("mounts the sidebar toggle on the sidebar edge and updates its direction", async () => {
  window.localStorage.setItem("mkvo.sidebar.collapsed", "false");
  const user = userEvent.setup();
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Dashboard content</div>} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 })
  });

  const collapse = screen.getByRole("button", { name: "Collapse sidebar" });
  expect(collapse).toHaveClass("sidebar-collapse-button");
  expect(collapse).toHaveAttribute("data-tooltip", "Collapse sidebar");
  await user.click(collapse);
  const expand = screen.getByRole("button", { name: "Expand sidebar" });
  expect(expand).toHaveAttribute("data-tooltip", "Expand sidebar");
  expect(window.localStorage.getItem("mkvo.sidebar.collapsed")).toBe("true");
});

it("does not toggle the sidebar when its edge is clicked outside the toggle", () => {
  window.localStorage.setItem("mkvo.sidebar.collapsed", "false");
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Dashboard content</div>} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 })
  });

  const sidebar = screen.getByRole("complementary");
  vi.spyOn(sidebar, "getBoundingClientRect").mockReturnValue({ x: 0, y: 0, width: 236, height: 900, top: 0, right: 236, bottom: 900, left: 0, toJSON: () => ({}) });
  fireEvent.click(sidebar, { clientX: 234 });
  expect(screen.getByRole("button", { name: "Collapse sidebar" })).toBeInTheDocument();
  expect(window.localStorage.getItem("mkvo.sidebar.collapsed")).toBe("false");
});

it("folds the sidebar when the window narrows and restores the saved choice when it widens", () => {
  window.localStorage.setItem("mkvo.sidebar.collapsed", "false");
  const listeners = new Set<() => void>();
  const narrow = { matches: false, addEventListener: (_: string, listener: () => void) => listeners.add(listener), removeEventListener: (_: string, listener: () => void) => listeners.delete(listener) };
  vi.stubGlobal("matchMedia", (query: string) => query === "(max-width: 1099px)" ? narrow : { matches: false, addEventListener: () => undefined, removeEventListener: () => undefined });
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Dashboard content</div>} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 })
  });

  narrow.matches = true;
  act(() => listeners.forEach((listener) => listener()));
  expect(screen.getByRole("button", { name: "Expand sidebar" })).toBeInTheDocument();
  expect(window.localStorage.getItem("mkvo.sidebar.collapsed")).toBe("false");

  narrow.matches = false;
  act(() => listeners.forEach((listener) => listener()));
  expect(screen.getByRole("button", { name: "Collapse sidebar" })).toBeInTheDocument();
  vi.unstubAllGlobals();
});

it("lets go of a remembered job the server no longer knows", async () => {
  window.sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify({ id: "gone-job", label: "Reorder Tracks" }));
  const getOperationJob = vi.fn(() => Promise.reject(new ApiError("Job not found", { code: "not_found", status: 404 })));
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Dashboard content</div>} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 }),
    getOperationJob
  });

  expect(await screen.findByText("ready")).toBeInTheDocument();
  expect(screen.queryByText(/checking job/)).not.toBeInTheDocument();
  expect(window.sessionStorage.getItem("mkvo.web.activeOperationJob")).toBeNull();
  expect(getOperationJob).toHaveBeenCalledTimes(1);
});

it.each([
  ["a proxy's bare 404", new ApiError("404 Not Found", { code: "HTTP_404", status: 404 })],
  ["a server error", new ApiError("Internal error", { code: "internal", status: 500 })]
])("keeps a remembered job through %s", async (_, error) => {
  vi.useFakeTimers({ shouldAdvanceTime: true });
  window.sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify({ id: "live-job", label: "Reorder Tracks" }));
  const getOperationJob = vi.fn(() => Promise.reject(error));
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Dashboard content</div>} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 }),
    getOperationJob
  });

  await waitFor(() => expect(getOperationJob).toHaveBeenCalledTimes(1));
  await act(() => vi.advanceTimersByTimeAsync(1100));
  expect(getOperationJob).toHaveBeenCalledTimes(2);
  expect(screen.getAllByText(/Reorder Tracks: checking job/).length).toBeGreaterThan(0);
  expect(window.sessionStorage.getItem("mkvo.web.activeOperationJob")).not.toBeNull();
  vi.useRealTimers();
});

it("opens folded in a narrow window even when the saved choice is expanded", () => {
  window.localStorage.setItem("mkvo.sidebar.collapsed", "false");
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: query === "(max-width: 1099px)", addEventListener: () => undefined, removeEventListener: () => undefined }));
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<div>Dashboard content</div>} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 })
  });
  expect(screen.getByRole("button", { name: "Expand sidebar" })).toBeInTheDocument();
  vi.unstubAllGlobals();
});
