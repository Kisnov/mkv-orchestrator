import { useMediaLibrary, MediaLibraryProvider } from "../state/MediaLibraryContext";
import type { BackendClient } from "../backend/client";
import { renderWithBackend } from "../test/render";
import type { MediaFileRow, OperationJobResponse, PropEditPreviewRequest, PropEditPreviewResponse, PropEditTemplateRequest, PropEditTemplateResponse, WebSettings } from "../generated/contracts";
import { act, screen, waitFor, within } from "@testing-library/react";
import { useQueryClient } from "@tanstack/react-query";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { TrackPropertiesPage } from "./TrackPropertiesPage";

function file(name: string): MediaFileRow {
  return {
    path: `/media/${name}`,
    fileName: name,
    extension: ".mkv",
    status: "Scanned",
    reader: "mkvmerge",
    codec: "HEVC/H.265",
    resolution: "1920x1080",
    bitDepth: "10",
    hdr: "None",
    videoSummary: "HEVC/H.265",
    audioSummary: "eng x1",
    subtitleSummary: "eng x1",
    attachmentSummary: "None",
    tracks: [],
    attachments: []
  };
}

function template(path: string): PropEditTemplateResponse {
  return {
    templatePath: path,
    templateFileName: path.split("/").pop() ?? path,
    audioTracks: [],
    subtitleTracks: [],
    defaultAudio: "Keep existing",
    forcedAudio: "Keep existing",
    defaultSubtitle: "Keep existing",
    forcedSubtitle: "Keep existing"
  };
}

function editJob(status: OperationJobResponse["status"]): OperationJobResponse {
  return {
    id: "job-1", kind: "PropertyEdit", status, createdUtc: "2026-08-25T20:00:00Z", startedUtc: "2026-08-25T20:00:01Z", completedUtc: null,
    completed: 1, failed: 0, skipped: 0, total: 1, currentFile: "", currentFilePercent: 100, lines: [], muxResult: null, propEditResult: null, error: ""
  };
}

function Rescan() {
  const queryClient = useQueryClient();
  return <button type="button" onClick={() => void queryClient.invalidateQueries({ queryKey: ["current-scan-files"] })}>Rescan</button>;
}

function SelectSecondTemplate({ path }: { path: string }) {
  const { setTemplateFilePath } = useMediaLibrary();
  return <button type="button" onClick={() => setTemplateFilePath(path)}>Set second template</button>;
}

beforeEach(() => {
  window.localStorage.clear();
  window.sessionStorage.clear();
});

describe("Track Properties template synchronization", () => {
  it("offers descriptive channel names separately from metadata-generated audio names", async () => {
    const scannedFile = file("Episode 01.mkv");
    const loadedTemplate = template(scannedFile.path);
    loadedTemplate.audioTracks = [
      {
        trackNumber: 1,
        trackLabel: "Audio 1",
        type: "audio",
        currentName: "",
        currentLanguage: "eng",
        currentCodec: "AAC",
        currentChannels: 6,
        currentDefault: true,
        editedName: "",
        nameFromMetadata: true,
        editedLanguage: "eng"
      },
      {
        trackNumber: 2,
        trackLabel: "Audio 2",
        type: "audio",
        currentName: "",
        currentLanguage: "eng",
        currentCodec: "AC-3",
        currentChannels: 2,
        currentDefault: false,
        editedName: "",
        nameFromMetadata: true,
        editedLanguage: "eng"
      }
    ];

    renderWithBackend(
      <MediaLibraryProvider>
        <TrackPropertiesPage />
      </MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-30T20:00:00Z", files: [scannedFile], selectedPaths: [scannedFile.path], summary: { total: 1, mkv: 1, mp4: 0, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate: () => Promise.resolve(loadedTemplate)
      }
    );

    const metadataOptions = await screen.findAllByRole("option", { name: "Auto Sort: [codec] [language] [channel]" });
    expect(metadataOptions).toHaveLength(2);
    metadataOptions.forEach((option) => expect(option).toHaveValue("__mkvo_metadata_track_name__"));
    const codecChannelOptions = screen.getAllByRole("option", { name: "Auto Sort: [codec] [channel]" });
    expect(codecChannelOptions).toHaveLength(2);
    codecChannelOptions.forEach((option) => expect(option).toHaveValue("__mkvo_codec_channel_track_name__"));
    const channelOptions = screen.getAllByRole("option", { name: "Auto Sort: [channel]" });
    expect(channelOptions).toHaveLength(2);
    channelOptions.forEach((option) => expect(option).toHaveValue("__mkvo_channel_track_name__"));
    expect(screen.getAllByRole("option", { name: "Use episode title" })).toHaveLength(2);
    expect(screen.getByRole("option", { name: "Custom title" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Custom name" })).toBeInTheDocument();
    const autoChannelOption = channelOptions[0];
    await userEvent.setup().selectOptions(autoChannelOption.closest("select")!, "__mkvo_channel_track_name__");
    await waitFor(() => {
      const saved = JSON.parse(window.sessionStorage.getItem("mkvo.web.trackPropertiesConfiguration")!);
      expect(saved.audioTracks[0].nameFromChannels).toBe(true);
      expect(saved.audioTracks[0].nameFromMetadata).toBe(false);
    });
    await userEvent.setup().selectOptions(autoChannelOption.closest("select")!, "__mkvo_codec_channel_track_name__");
    await waitFor(() => {
      const saved = JSON.parse(window.sessionStorage.getItem("mkvo.web.trackPropertiesConfiguration")!);
      expect(saved.audioTracks[0].nameFromCodecChannels).toBe(true);
      expect(saved.audioTracks[0].nameFromChannels).toBe(false);
      expect(saved.audioTracks[0].nameFromMetadata).toBe(false);
    });
  });

  it("defaults audio and subtitle forced-track selections to None", async () => {
    const scannedFile = file("Episode 01.mkv");
    const loadedTemplate = template(scannedFile.path);
    loadedTemplate.forcedAudio = "";
    loadedTemplate.forcedSubtitle = "";

    renderWithBackend(
      <MediaLibraryProvider>
        <TrackPropertiesPage />
      </MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-25T20:00:00Z", files: [scannedFile], selectedPaths: [scannedFile.path], summary: { total: 1, mkv: 1, mp4: 0, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate: () => Promise.resolve(loadedTemplate)
      }
    );

    await screen.findByText(`Template loaded: ${scannedFile.fileName}`);
    const forcedTrackSelections = screen.getAllByRole("combobox", { name: "Set forced track" });
    expect(forcedTrackSelections).toHaveLength(2);
    forcedTrackSelections.forEach((selection) => expect(selection).toHaveValue("None"));
  });

  it("can set or clear the first video track default flag", async () => {
    const user = userEvent.setup();
    const scannedFile = file("Episode 01.mkv");
    const loadedTemplate = template(scannedFile.path);
    const buildPropEditPreview = vi.fn(() => Promise.resolve({
      actions: [], skipped: [], noChange: [], summary: "No changes", status: "No changes",
      planId: null, planFingerprint: null, idempotencyKey: null
    }));

    renderWithBackend(
      <MediaLibraryProvider><TrackPropertiesPage /></MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-25T20:00:00Z", files: [scannedFile], selectedPaths: [scannedFile.path], summary: { total: 1, mkv: 1, mp4: 0, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate: () => Promise.resolve(loadedTemplate),
        buildPropEditPreview
      }
    );

    await screen.findByText(`Template loaded: ${scannedFile.fileName}`);
    const videoDefault = screen.getByRole("combobox", { name: "Set video default flag" });
    expect(within(videoDefault).getByRole("option", { name: "Default" })).toBeInTheDocument();
    expect(within(videoDefault).getByRole("option", { name: "None" })).toBeInTheDocument();

    await user.selectOptions(videoDefault, "Default");
    await user.click(screen.getByRole("button", { name: "Preview Summary" }));
    await waitFor(() => expect(buildPropEditPreview).toHaveBeenLastCalledWith(expect.objectContaining({ selectedDefaultVideo: "Default" })));

    await user.selectOptions(videoDefault, "None");
    await user.click(screen.getByRole("button", { name: "Preview Summary" }));
    await waitFor(() => expect(buildPropEditPreview).toHaveBeenLastCalledWith(expect.objectContaining({ selectedDefaultVideo: "None" })));
  });

  it("applies the plan that was previewed while the request is unchanged", async () => {
    const user = userEvent.setup();
    const scannedFile = file("Episode 01.mkv");
    const buildPropEditPreview = vi.fn<(request: PropEditPreviewRequest) => Promise<PropEditPreviewResponse>>((): Promise<PropEditPreviewResponse> => Promise.resolve({
      actions: [{ index: 0, filePath: scannedFile.path, fileName: scannedFile.fileName, description: "Apply 1 property mutation(s)", command: "" }],
      skipped: [], noChange: [], summary: "ready", status: "ready",
      planId: `plan-${buildPropEditPreview.mock.calls.length}`, planFingerprint: "fingerprint", idempotencyKey: `key-${buildPropEditPreview.mock.calls.length}`
    }));
    const startPropEditApply = vi.fn<(request: PropEditPreviewRequest) => Promise<never>>(() => Promise.reject(new Error("Test stops before tracking a job")));

    renderWithBackend(
      <MediaLibraryProvider><TrackPropertiesPage /></MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-25T20:00:00Z", files: [scannedFile], selectedPaths: [scannedFile.path], summary: { total: 1, mkv: 1, mp4: 0, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate: () => Promise.resolve(template(scannedFile.path)),
        buildPropEditPreview,
        startPropEditApply
      }
    );

    await screen.findByText(`Template loaded: ${scannedFile.fileName}`);
    const applyButton = screen.getByRole("button", { name: "Apply Changes" });
    await user.click(screen.getByRole("button", { name: "Preview Summary" }));
    await waitFor(() => expect(buildPropEditPreview).toHaveBeenCalledTimes(1));
    await user.click(screen.getByTitle("Close"));
    await user.click(applyButton);
    await waitFor(() => expect(startPropEditApply).toHaveBeenCalledTimes(1));
    expect(buildPropEditPreview).toHaveBeenCalledTimes(1);
    expect(startPropEditApply.mock.calls[0][0]).toMatchObject({ planId: "plan-1", idempotencyKey: "key-1" });

    // A plan is spent once applied, and a changed request needs a new one.
    await user.click(applyButton);
    await waitFor(() => expect(startPropEditApply).toHaveBeenCalledTimes(2));
    expect(startPropEditApply.mock.calls[1][0]).toMatchObject({ planId: "plan-2" });
    await user.click(screen.getByRole("button", { name: "Preview Summary" }));
    await waitFor(() => expect(buildPropEditPreview).toHaveBeenCalledTimes(3));
    await user.click(screen.getByTitle("Close"));
    await user.selectOptions(screen.getByRole("combobox", { name: "Set video default flag" }), "Default");
    await user.click(applyButton);
    await waitFor(() => expect(startPropEditApply).toHaveBeenCalledTimes(3));
    expect(buildPropEditPreview).toHaveBeenCalledTimes(4);
    expect(startPropEditApply.mock.calls[2][0]).toMatchObject({ planId: "plan-4", selectedDefaultVideo: "Default" });
  });

  it("reads the template again once a running job finishes", async () => {
    const scannedFile = file("Episode 01.mkv");
    window.sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify({ id: "job-1", label: "Edit Tracks" }));
    const loadPropEditTemplate = vi.fn(() => Promise.resolve(template(scannedFile.path)));
    let status: OperationJobResponse["status"] = "Running";

    renderWithBackend(
      <MediaLibraryProvider><TrackPropertiesPage /></MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-25T20:00:00Z", files: [scannedFile], selectedPaths: [scannedFile.path], summary: { total: 1, mkv: 1, mp4: 0, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate,
        getOperationJob: () => Promise.resolve(editJob(status))
      }
    );

    await screen.findByText("Applying 1/1");
    await waitFor(() => expect(loadPropEditTemplate).toHaveBeenCalledTimes(1));
    status = "Completed";
    await waitFor(() => expect(loadPropEditTemplate).toHaveBeenCalledTimes(2), { timeout: 3000 });
  });

  it("does not read the template again when returning after a finished job", async () => {
    const user = userEvent.setup();
    const scannedFile = file("Episode 01.mkv");
    window.sessionStorage.setItem("mkvo.web.activeOperationJob", JSON.stringify({ id: "job-1", label: "Edit Tracks" }));
    const loadPropEditTemplate = vi.fn(() => Promise.resolve(template(scannedFile.path)));
    function Visit() {
      const [shown, setShown] = useState(true);
      return <><button type="button" onClick={() => setShown((value) => !value)}>Toggle page</button>{shown ? <TrackPropertiesPage /> : null}</>;
    }

    renderWithBackend(
      <MediaLibraryProvider><Visit /></MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-25T20:00:00Z", files: [scannedFile], selectedPaths: [scannedFile.path], summary: { total: 1, mkv: 1, mp4: 0, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate,
        getOperationJob: () => Promise.resolve(editJob("Completed"))
      }
    );

    const settle = () => act(() => new Promise((resolve) => setTimeout(resolve, 100)));
    await screen.findByText(`Template loaded: ${scannedFile.fileName}`);
    await settle();
    const reads = loadPropEditTemplate.mock.calls.length;

    await user.click(screen.getByRole("button", { name: "Toggle page" }));
    await user.click(screen.getByRole("button", { name: "Toggle page" }));
    await screen.findByText(`Template loaded: ${scannedFile.fileName}`);
    await settle();
    expect(loadPropEditTemplate).toHaveBeenCalledTimes(reads);
  });

  it("never takes a non-MKV file as the template", async () => {
    const user = userEvent.setup();
    const movie = { ...file("Episode 00.mp4"), extension: ".mp4" };
    const first = file("Episode 01.mkv");
    const second = file("Episode 02.mkv");
    const files = [movie, first, second];
    window.sessionStorage.setItem("mkvo.web.scannedFiles", JSON.stringify(files));
    window.sessionStorage.setItem("mkvo.web.templateFilePath", movie.path);
    const loadPropEditTemplate = vi.fn((request: PropEditTemplateRequest) => Promise.resolve(template(request.templatePath ?? "")));
    const buildPropEditPreview = vi.fn<(request: PropEditPreviewRequest) => Promise<PropEditPreviewResponse>>(() => Promise.resolve({
      actions: [], skipped: [], noChange: [], summary: "No changes", status: "No changes", planId: null, planFingerprint: null, idempotencyKey: null
    }));

    renderWithBackend(
      <MediaLibraryProvider><TrackPropertiesPage /></MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-25T20:00:00Z", files, selectedPaths: [], summary: { total: 3, mkv: 2, mp4: 1, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate,
        buildPropEditPreview
      }
    );

    await screen.findByText(`Template loaded: ${first.fileName}`);
    expect(screen.getByRole("combobox", { name: /template file/i })).toHaveValue(first.path);
    expect(loadPropEditTemplate).not.toHaveBeenCalledWith(expect.objectContaining({ templatePath: movie.path }));
    await user.click(screen.getByRole("button", { name: "Preview Summary" }));
    await waitFor(() => expect(buildPropEditPreview).toHaveBeenCalled());
    expect(buildPropEditPreview.mock.calls[0][0]).toMatchObject({ templatePath: first.path, selectedPaths: [first.path, second.path] });
  });

  it("reloads its track template when Dashboard changes the shared template file", async () => {
    const user = userEvent.setup();
    const first = file("Episode 01.mkv");
    const second = file("Episode 02.mkv");
    const files = [first, second];
    window.sessionStorage.setItem("mkvo.web.scannedFiles", JSON.stringify(files));
    window.sessionStorage.setItem("mkvo.web.templateFilePath", first.path);
    const loadPropEditTemplate = vi.fn((request: PropEditTemplateRequest) => Promise.resolve(template(request.templatePath ?? "")));

    renderWithBackend(
      <MediaLibraryProvider>
        <SelectSecondTemplate path={second.path} />
        <TrackPropertiesPage />
      </MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-25T20:00:00Z", files, selectedPaths: files.map((item) => item.path), summary: { total: 2, mkv: 2, mp4: 0, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate
      }
    );

    await waitFor(() => expect(loadPropEditTemplate).toHaveBeenCalledWith({ files, templatePath: first.path }));
    await user.click(screen.getByRole("button", { name: "Set second template" }));
    await waitFor(() => expect(loadPropEditTemplate).toHaveBeenCalledWith({ files, templatePath: second.path }));
    expect(screen.getByRole("combobox", { name: /template file/i })).toHaveValue(second.path);
    expect(await screen.findByText(`Template loaded: ${second.fileName}`)).toBeInTheDocument();
  });
});

describe("Edit Tracks file selection", () => {
  type Track = MediaFileRow["tracks"][number];
  const video: Track = { id: 0, trackNumber: 1, type: "video", codec: "HEVC", language: "und", name: "", channels: null, default: true, forced: false };
  const catalan: Track = { id: 1, trackNumber: 2, type: "audio", codec: "AAC", language: "cat", name: "English", channels: 2, default: true, forced: false };
  const japanese: Track = { id: 2, trackNumber: 3, type: "audio", codec: "AAC", language: "jpn", name: "Japonès", channels: 2, default: false, forced: false };

  function episode(name: string, tracks: Track[]): MediaFileRow {
    return { ...file(name), audioSummary: "cat, jpn", subtitleSummary: "None", tracks };
  }

  // 01-05 carry the mislabelled audio track; 06-10 differ from them in one way each.
  const equal = ["01", "02", "03", "04", "05"].map((number) => episode(`Episode ${number}.mkv`, [video, catalan, japanese]));
  const fixed = { ...catalan, name: "Català" };
  const differing = [
    episode("Episode 06.mkv", [video, fixed, japanese]),
    episode("Episode 07.mkv", [video, fixed, japanese]),
    episode("Episode 08.mkv", [video, catalan]),
    { ...episode("Episode 09.mkv", [video, catalan, japanese]), codec: "AVC" },
    // Same tracks in another order: the case an edit by position gets wrong.
    episode("Episode 10.mkv", [video, { ...japanese, id: 1, trackNumber: 2 }, { ...catalan, id: 2, trackNumber: 3 }])
  ];
  const scanned = [...equal, ...differing];
  const equalPaths = equal.map((item) => item.path);

  function audioRow(trackNumber: number, name: string, language: string) {
    return { trackNumber, trackLabel: `Audio ${trackNumber}`, type: "audio", currentName: name, currentLanguage: language, currentCodec: "AAC", currentChannels: 2, currentDefault: trackNumber === 1, editedName: name, nameFromMetadata: false, editedLanguage: language };
  }

  function loaded(request: PropEditTemplateRequest) {
    return { ...template(request.templatePath ?? ""), audioTracks: [audioRow(1, "English", "cat"), audioRow(2, "Japonès", "jpn")] };
  }

  function renderPage(overrides: Partial<BackendClient> = {}) {
    const plan = vi.fn((request: PropEditPreviewRequest) => Promise.resolve({
      actions: request.selectedPaths.map((path, index) => ({ index, filePath: path, fileName: path.split("/").pop()!, description: "Apply 1 property mutation(s)", command: "" })),
      skipped: [], noChange: [], summary: "ready", status: "Track-properties preview ready",
      planId: `plan-${plan.mock.calls.length}`, planFingerprint: "fingerprint", idempotencyKey: `key-${plan.mock.calls.length}`
    }));
    const apply = vi.fn<(request: PropEditPreviewRequest) => Promise<never>>(() => Promise.reject(new Error("Test stops before tracking a job")));
    renderWithBackend(
      <MediaLibraryProvider><Rescan /><TrackPropertiesPage /></MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-11-12T10:00:00Z", files: scanned, selectedPaths: [], summary: { total: 10, mkv: 10, mp4: 0, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate: (request: PropEditTemplateRequest) => Promise.resolve(loaded(request)),
        buildPropEditPreview: plan,
        startPropEditApply: apply,
        ...overrides
      }
    );
    return { plan, apply };
  }

  function checked() {
    return screen.getAllByRole("checkbox", { name: /^Select Episode/ }).filter((box) => (box as HTMLInputElement).checked).map((box) => box.getAttribute("aria-label")!.replace("Select ", ""));
  }

  async function ready() {
    const applyButton = screen.getByRole("button", { name: "Apply Changes" });
    await waitFor(() => expect(applyButton).toBeEnabled());
    return applyButton;
  }

  it("selects the template and the files equal to it, and sends only those paths", async () => {
    const user = userEvent.setup();
    const { plan, apply } = renderPage();
    const applyButton = await ready();

    expect(checked()).toEqual(equal.map((item) => item.fileName));
    expect(screen.getByText("5 files differ from the template and are not selected.")).toBeInTheDocument();
    expect(screen.getByText("5 files selected")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Preview Summary" }));
    await waitFor(() => expect(plan).toHaveBeenCalledTimes(1));
    expect(plan.mock.calls[0][0].selectedPaths).toEqual(equalPaths);
    await user.click(screen.getByTitle("Close"));

    await user.click(applyButton);
    await waitFor(() => expect(apply).toHaveBeenCalledTimes(1));
    expect(apply.mock.calls[0][0].selectedPaths).toEqual(equalPaths);
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("recomputes the default selection when the template changes", async () => {
    const user = userEvent.setup();
    const { plan } = renderPage();
    await ready();

    await user.click(screen.getByRole("checkbox", { name: "Select Episode 09.mkv" }));
    await user.selectOptions(screen.getByRole("combobox", { name: /template file/i }), differing[0].path);
    await screen.findByText("Template loaded: Episode 06.mkv");

    expect(checked()).toEqual(["Episode 06.mkv", "Episode 07.mkv"]);
    expect(screen.getByText("8 files differ from the template and are not selected.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Preview Summary" }));
    await waitFor(() => expect(plan).toHaveBeenCalledTimes(1));
    expect(plan.mock.calls[0][0]).toMatchObject({ templatePath: differing[0].path, selectedPaths: [differing[0].path, differing[1].path] });
  });

  it("blocks preview and apply until the newly chosen template has loaded", async () => {
    const user = userEvent.setup();
    let finishRead = () => {};
    const { plan, apply } = renderPage({
      loadPropEditTemplate: (request) => request.templatePath === differing[0].path
        ? new Promise((resolve) => { finishRead = () => resolve(loaded(request)); })
        : Promise.resolve(loaded(request))
    });
    const applyButton = await ready();
    const previewButton = screen.getByRole("button", { name: "Preview Summary" });

    await user.selectOptions(screen.getByRole("combobox", { name: /template file/i }), differing[0].path);
    expect(checked()).toEqual(["Episode 06.mkv", "Episode 07.mkv"]);
    expect(applyButton).toBeDisabled();
    expect(previewButton).toBeDisabled();
    await user.click(applyButton);
    await user.click(previewButton);
    expect(plan).not.toHaveBeenCalled();
    expect(apply).not.toHaveBeenCalled();

    await act(async () => finishRead());
    await waitFor(() => expect(applyButton).toBeEnabled());
    expect(previewButton).toBeEnabled();
  });

  it("asks for confirmation, naming the target tracks, before applying to a file that differs", async () => {

    const user = userEvent.setup();
    const { plan, apply } = renderPage();
    const applyButton = await ready();

    await user.click(screen.getByRole("checkbox", { name: "Select Episode 10.mkv" }));
    expect(screen.getByText(/1 selected file differs from the template/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Preview Summary" }));
    const summary = await screen.findByText(/Audio 1 → track ID 1 · jpn · AAC · Japonès \(template: track ID 1 · cat · AAC · English\)/);
    expect(summary).toHaveTextContent("Audio 2 → track ID 2 · cat · AAC · English (template: track ID 2 · jpn · AAC · Japonès)");
    await user.click(screen.getByTitle("Close"));

    await user.click(applyButton);
    const dialog = await screen.findByRole("dialog", { name: "Apply to files that differ from the template?" });
    expect(apply).not.toHaveBeenCalled();
    expect(within(dialog).getByText("Episode 10.mkv")).toBeInTheDocument();
    expect(within(dialog).getByText(/Audio 1 → track ID 1 · jpn · AAC · Japonès/)).toBeInTheDocument();
    expect(within(dialog).queryByText("Episode 01.mkv")).not.toBeInTheDocument();

    await user.click(within(dialog).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(apply).not.toHaveBeenCalled();

    await user.click(applyButton);
    await user.click(within(await screen.findByRole("dialog")).getByRole("button", { name: "Apply to 6 file(s)" }));
    await waitFor(() => expect(apply).toHaveBeenCalledTimes(1));
    expect(apply.mock.calls[0][0]).toMatchObject({ planId: "plan-1", selectedPaths: [...equalPaths, differing[4].path] });
    expect(plan).toHaveBeenCalledTimes(1);
  });

  it("keeps confirming the files of the pending request when the scan changes underneath", async () => {
    const user = userEvent.setup();
    let scan = { updatedUtc: "2026-11-12T10:00:00Z", files: scanned };
    const { apply } = renderPage({
      getCurrentScanFiles: () => Promise.resolve({ ...scan, selectedPaths: [], summary: { total: 10, mkv: 10, mp4: 0, failed: 0, cached: 0 } })
    });
    const applyButton = await ready();

    await user.click(screen.getByRole("checkbox", { name: "Select Episode 10.mkv" }));
    await user.click(applyButton);
    const dialog = await screen.findByRole("dialog");

    // The rescan finds Episode 10 equal to the template, so the page stops counting it as differing.
    scan = { updatedUtc: "2026-11-12T10:05:00Z", files: scanned.map((item) => item === differing[4] ? { ...item, tracks: equal[0].tracks } : item) };
    await user.click(screen.getByRole("button", { name: "Rescan" }));
    await screen.findByText("4 files differ from the template and are not selected.");

    expect(within(dialog).getByText(/1 selected file differs/)).toBeInTheDocument();
    expect(within(dialog).getByText("Episode 10.mkv")).toBeInTheDocument();
    await user.click(within(dialog).getByRole("button", { name: "Apply to 6 file(s)" }));
    await waitFor(() => expect(apply).toHaveBeenCalledTimes(1));
    expect(apply.mock.calls[0][0].selectedPaths).toEqual([...equalPaths, differing[4].path]);
  });

  it("disables preview and apply when nothing is selected", async () => {

    const user = userEvent.setup();
    const { plan, apply } = renderPage();
    const applyButton = await ready();

    await user.click(screen.getByRole("button", { name: "Select none" }));
    expect(applyButton).toBeDisabled();
    expect(screen.getByRole("button", { name: "Preview Summary" })).toBeDisabled();
    expect(screen.getByText("0 files selected")).toBeInTheDocument();
    await user.click(applyButton);
    expect(plan).not.toHaveBeenCalled();
    expect(apply).not.toHaveBeenCalled();
  });
});
