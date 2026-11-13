import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, useLocation } from "react-router-dom";
import { beforeEach, expect, it, vi } from "vitest";

import App from "../App";
import { createMockBackendClient } from "../backend/mockClient";
import { setBackendClient } from "../backend/runtime";
import type { MediaFileRow } from "../generated/contracts";

vi.mock("./TrackPropertiesPage", () => ({ TrackPropertiesPage: () => <p>Edit Tracks page</p> }));
vi.mock("./ReorderTracksPage", () => ({ ReorderTracksPage: () => <p>Reorder page</p> }));
vi.mock("./SubtitleFormatPage", () => ({ SubtitleFormatPage: () => <p>Subtitle Format page</p> }));
vi.mock("./MuxRemuxPage", () => ({ MuxRemuxPage: ({ workflow }: { workflow: string }) => <p>Mux page: {workflow}</p> }));

const status = { name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 };

function Location() { return <output aria-label="Location">{useLocation().pathname}</output>; }

function renderAt(path: string, files: MediaFileRow[] = []) {
  setBackendClient(createMockBackendClient({
    transport: "tauri",
    getStatus: () => Promise.resolve(status),
    getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-11-13T10:00:00Z", files, selectedPaths: [], summary: { total: files.length, mkv: 0, mp4: files.length, failed: 0, cached: 0 } })
  }));
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <MemoryRouter initialEntries={[path]}><App /><Location /></MemoryRouter>
    </QueryClientProvider>
  );
}

beforeEach(() => {
  localStorage.clear();
  sessionStorage.clear();
});

it.each([
  ["/tracks", "/tracks/properties", "Edit Tracks page"],
  ["/edit-tracks", "/tracks/properties", "Edit Tracks page"],
  ["/track-properties", "/tracks/properties", "Edit Tracks page"],
  ["/reorder-tracks", "/tracks/reorder", "Reorder page"],
  ["/subtitle-format", "/tracks/subtitle-format", "Subtitle Format page"],
  ["/remove-tracks", "/tracks/remove", "Mux page: remove"],
  ["/mux-remux", "/tracks/remove", "Mux page: remove"],
  ["/convert-remux", "/tracks/convert", "Mux page: convert"],
  ["/tracks/unknown", "/tracks/properties", "Edit Tracks page"]
])("sends %s to %s", async (from, to, page) => {
  renderAt(from);
  expect(await screen.findByText(page)).toBeInTheDocument();
  expect(screen.getByLabelText("Location")).toHaveTextContent(to);
  expect(screen.getByRole("link", { name: "Tracks" })).toHaveAttribute("aria-current", "page");
});

it("keeps Subtitles as its own page", async () => {
  renderAt("/subtitles");
  expect(await screen.findByText("Mux page: subtitles")).toBeInTheDocument();
  expect(screen.queryByRole("navigation", { name: "Track operations" })).not.toBeInTheDocument();
});

it("switches operations from the tabs and offers Convert / Remux only with MP4 files", async () => {
  const user = userEvent.setup();
  renderAt("/tracks/properties");
  const tabs = await screen.findByRole("navigation", { name: "Track operations" });
  expect(within(tabs).getAllByRole("link").map((link) => link.textContent)).toEqual(["Edit Tracks", "Reorder", "Subtitle Format", "Remove Tracks"]);
  expect(within(tabs).getByRole("link", { name: "Edit Tracks" })).toHaveAttribute("aria-current", "page");

  await user.click(within(tabs).getByRole("link", { name: "Reorder" }));
  expect(await screen.findByText("Reorder page")).toBeInTheDocument();
  expect(screen.getByLabelText("Location")).toHaveTextContent("/tracks/reorder");
});

it("shows the Convert / Remux tab once an MP4 is scanned", async () => {
  const mp4 = { path: "/media/Movie.mp4", fileName: "Movie.mp4", extension: ".mp4", tracks: [], attachments: [] } as unknown as MediaFileRow;
  renderAt("/tracks/remove", [mp4]);
  const tabs = await screen.findByRole("navigation", { name: "Track operations" });
  expect(await within(tabs).findByRole("link", { name: "Convert / Remux" })).toHaveAttribute("href", "/tracks/convert");
});
