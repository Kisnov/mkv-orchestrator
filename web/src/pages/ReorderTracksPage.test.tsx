import { useEffect } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect, it, vi } from "vitest";

import { ReorderTracksPage } from "./ReorderTracksPage";
import { MediaLibraryProvider, useMediaLibrary } from "../state/MediaLibraryContext";
import { OperationJobProvider } from "../state/OperationJobContext";
import { renderWithBackend } from "../test/render";
import type { MediaFileRow, MuxPreviewRequest } from "../api";

const tracks = [
  { id: 0, trackNumber: 1, type: "video", codec: "HEVC", language: "und", name: "", channels: null, default: true, forced: false },
  { id: 1, trackNumber: 2, type: "subtitles", codec: "ASS", language: "eng", name: "Dialogue", channels: null, default: true, forced: false },
  { id: 2, trackNumber: 3, type: "subtitles", codec: "ASS", language: "eng", name: "Signs/Songs", channels: null, default: false, forced: false }
];

function file(name: string): MediaFileRow {
  return { path: `/media/${name}`, fileName: name, extension: ".mkv", status: "Scanned", reader: "mkvmerge", codec: "HEVC", resolution: "1080p", bitDepth: "10", hdr: "", videoSummary: "", audioSummary: "", subtitleSummary: "eng x2", attachmentSummary: "", tracks, attachments: [] };
}

function Scan({ files }: { files: MediaFileRow[] }) {
  const library = useMediaLibrary();
  useEffect(() => {
    library.setFiles(files);
    library.setSelectedPaths(files.map((item) => item.path));
  }, []);
  return null;
}

it("builds a batch request from the user-arranged template order", async () => {
  localStorage.clear(); sessionStorage.clear();
  const files = [file("01.mkv"), file("02.mkv")];
  const buildMuxPreview = vi.fn((_request: MuxPreviewRequest) => Promise.resolve({ actions: [], noChangeFiles: [], summary: "ready", status: "ready", planId: null, planFingerprint: null, idempotencyKey: null }));
  const user = userEvent.setup();
  renderWithBackend(
    <MediaLibraryProvider><OperationJobProvider><Scan files={files}/><ReorderTracksPage/></OperationJobProvider></MediaLibraryProvider>,
    { buildMuxPreview, setFileSelection: (paths) => Promise.resolve({ files, selectedPaths: paths, updatedUtc: null, summary: { total: 2, mkv: 2, mp4: 0, failed: 0, cached: 0 } }) }
  );

  await screen.findByText("Dialogue");
  await user.click(screen.getByRole("button", { name: "Move Dialogue down" }));
  expect(screen.getByRole("heading", { name: "File Info" })).toBeInTheDocument();
  expect(screen.getByRole("checkbox", { name: "Select 01.mkv" })).toBeChecked();
  expect(screen.getByRole("checkbox", { name: "Select 02.mkv" })).toBeChecked();
  await user.click(screen.getByRole("button", { name: "Preview Summary" }));

  await waitFor(() => expect(buildMuxPreview).toHaveBeenCalled());
  expect(buildMuxPreview.mock.calls[0][0]).toMatchObject({
    reorderTracks: true,
    reorderTemplatePath: "/media/01.mkv",
    reorderTemplateTrackIds: [0, 2, 1],
    selectedPaths: ["/media/01.mkv", "/media/02.mkv"],
    preserveOriginal: false
  });
});

function Rescan() {
  const queryClient = useQueryClient();
  return <button type="button" onClick={() => void queryClient.invalidateQueries({ queryKey: ["current-scan-files"] })}>Rescan</button>;
}

it("keeps the user's batch when the scan changes", async () => {
  localStorage.clear(); sessionStorage.clear();
  const files = [file("01.mkv"), file("02.mkv"), file("03.mkv")];
  const summary = { total: 3, mkv: 3, mp4: 0, failed: 0, cached: 0 };
  let selected: string[] = [];
  const getCurrentScanFiles = vi.fn(() => Promise.resolve({ updatedUtc: `2026-11-12T10:00:0${getCurrentScanFiles.mock.calls.length}Z`, files, selectedPaths: selected, summary }));
  const user = userEvent.setup();
  renderWithBackend(
    <MediaLibraryProvider><Rescan/><ReorderTracksPage/></MediaLibraryProvider>,
    { getCurrentScanFiles, setFileSelection: (paths) => { selected = paths; return Promise.resolve({ files, selectedPaths: paths, updatedUtc: null, summary }); } }
  );

  // Nothing selected yet, so the page still starts with every MKV.
  await waitFor(() => expect(screen.getByRole("checkbox", { name: "Select 03.mkv" })).toBeChecked());
  await user.click(screen.getByRole("checkbox", { name: "Select 03.mkv" }));
  await waitFor(() => expect(selected).toEqual(["/media/01.mkv", "/media/02.mkv"]));

  await user.click(screen.getByRole("button", { name: "Rescan" }));
  await waitFor(() => expect(getCurrentScanFiles).toHaveBeenCalledTimes(2));
  await act(() => new Promise((resolve) => setTimeout(resolve, 50)));

  expect(screen.getByRole("checkbox", { name: "Select 03.mkv" })).not.toBeChecked();
  expect(selected).toEqual(["/media/01.mkv", "/media/02.mkv"]);
});

it("opens with every MKV in the batch whatever was selected before", async () => {
  localStorage.clear(); sessionStorage.clear();
  const files = [file("01.mkv"), file("02.mkv"), file("03.mkv")];
  const summary = { total: 3, mkv: 3, mp4: 0, failed: 0, cached: 0 };
  // A plain click on a Dashboard row leaves the shared selection at that one file.
  let selected = ["/media/02.mkv"];
  localStorage.setItem("mkvo.web.selectedPaths", JSON.stringify(selected));
  renderWithBackend(
    <MediaLibraryProvider><ReorderTracksPage/></MediaLibraryProvider>,
    {
      getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-11-12T10:00:00Z", files, selectedPaths: selected, summary }),
      setFileSelection: (paths) => { selected = paths; return Promise.resolve({ files, selectedPaths: paths, updatedUtc: null, summary }); }
    }
  );

  await waitFor(() => expect(screen.getByRole("checkbox", { name: "Select 03.mkv" })).toBeChecked());
  expect(screen.getByRole("checkbox", { name: "Select 01.mkv" })).toBeChecked();
  expect(screen.getByRole("button", { name: "Apply Changes" })).toBeEnabled();
  expect(selected).toEqual(["/media/01.mkv", "/media/02.mkv", "/media/03.mkv"]);
});

it("says why it cannot run when the template is not in the batch", async () => {
  localStorage.clear(); sessionStorage.clear();
  const files = [file("01.mkv"), file("02.mkv"), file("03.mkv")];
  const summary = { total: 3, mkv: 3, mp4: 0, failed: 0, cached: 0 };
  let selected: string[] = [];
  const getCurrentScanFiles = vi.fn(() => Promise.resolve({ updatedUtc: `2026-11-12T10:00:0${getCurrentScanFiles.mock.calls.length}Z`, files, selectedPaths: selected, summary }));
  const user = userEvent.setup();
  renderWithBackend(
    <MediaLibraryProvider><Rescan/><ReorderTracksPage/></MediaLibraryProvider>,
    { getCurrentScanFiles, setFileSelection: (paths) => { selected = paths; return Promise.resolve({ files, selectedPaths: paths, updatedUtc: null, summary }); } }
  );
  const applyButton = screen.getByRole("button", { name: "Apply Changes" });
  const explanation = "The template file is not in the batch. Check it in the list to continue.";
  await waitFor(() => expect(applyButton).toBeEnabled());
  expect(screen.queryByText(explanation)).not.toBeInTheDocument();

  // The backend reports a selection without the template, as another window could leave it.
  selected = ["/media/02.mkv"];
  await user.click(screen.getByRole("button", { name: "Rescan" }));
  await waitFor(() => expect(screen.getByRole("checkbox", { name: "Select 01.mkv" })).not.toBeChecked());
  expect(applyButton).toBeDisabled();
  expect(screen.getByRole("button", { name: "Preview Summary" })).toBeDisabled();
  expect(screen.getByText(explanation)).toBeInTheDocument();

  await user.click(screen.getByRole("checkbox", { name: "Select 01.mkv" }));
  await waitFor(() => expect(applyButton).toBeEnabled());
  expect(screen.queryByText(explanation)).not.toBeInTheDocument();
});
