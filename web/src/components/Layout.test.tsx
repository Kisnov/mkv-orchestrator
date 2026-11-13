import userEvent from "@testing-library/user-event";
import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { Route, Routes } from "react-router-dom";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { Layout } from "./Layout";
import { MediaLibraryProvider } from "../state/MediaLibraryContext";
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
