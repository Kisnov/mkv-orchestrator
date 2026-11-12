import { useMediaLibrary, MediaLibraryProvider } from "../state/MediaLibraryContext";
import { renderWithBackend } from "../test/render";
import type { MediaFileRow, PropEditPreviewRequest, PropEditTemplateRequest, PropEditTemplateResponse, WebSettings } from "../generated/contracts";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
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
    const buildPropEditPreview = vi.fn((_request: PropEditPreviewRequest) => Promise.resolve({
      actions: [{ index: 0, filePath: scannedFile.path, fileName: scannedFile.fileName, description: "Apply 1 property mutation(s)", command: "" }],
      skipped: [], noChange: [], summary: "ready", status: "ready",
      planId: `plan-${buildPropEditPreview.mock.calls.length}`, planFingerprint: "fingerprint", idempotencyKey: `key-${buildPropEditPreview.mock.calls.length}`
    }));
    const startPropEditApply = vi.fn((_request: PropEditPreviewRequest) => Promise.reject(new Error("Test stops before tracking a job")));

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

  function renderPage() {
    const plan = vi.fn((request: PropEditPreviewRequest) => Promise.resolve({
      actions: request.selectedPaths.map((path, index) => ({ index, filePath: path, fileName: path.split("/").pop()!, description: "Apply 1 property mutation(s)", command: "" })),
      skipped: [], noChange: [], summary: "ready", status: "Track-properties preview ready",
      planId: `plan-${plan.mock.calls.length}`, planFingerprint: "fingerprint", idempotencyKey: `key-${plan.mock.calls.length}`
    }));
    const apply = vi.fn((_request: PropEditPreviewRequest) => Promise.reject(new Error("Test stops before tracking a job")));
    renderWithBackend(
      <MediaLibraryProvider><TrackPropertiesPage /></MediaLibraryProvider>,
      {
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-11-12T10:00:00Z", files: scanned, selectedPaths: [], summary: { total: 10, mkv: 10, mp4: 0, failed: 0, cached: 0 } }),
        getWebSettings: () => Promise.resolve({ audioNamePresets: [], subtitleNamePresets: [], languagePresets: [] } as unknown as WebSettings),
        loadPropEditTemplate: (request: PropEditTemplateRequest) => Promise.resolve({ ...template(request.templatePath ?? ""), audioTracks: [audioRow(1, "English", "cat"), audioRow(2, "Japonès", "jpn")] }),
        buildPropEditPreview: plan,
        startPropEditApply: apply
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
