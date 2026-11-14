import { buildMuxPreview, startMuxApply, type MediaFileRow, type MuxPreviewRequest, type MuxPreviewResponse } from "../api";
import { useOperationRun, type OperationRunOptions } from "./useOperationRun";

type MuxOptions = Omit<MuxPreviewRequest, "files" | "selectedPaths" | "planId" | "planFingerprint" | "idempotencyKey">;

// Every mkvmerge step off: each page turns on only its own.
const muxDefaults: MuxOptions = {
  removeUnwantedAudioLanguages: false, keepAudioLanguages: "", removeUnwantedSubtitleLanguages: false, keepSubtitleLanguages: "", removeUnwantedTrackIds: false, removeTrackIdsText: "",
  preserveChapters: true, preserveAttachments: true, preserveOriginal: false, remuxOutputSuffix: ".remuxed",
  reorderTracks: false, reorderTemplatePath: "", reorderTemplateTrackIds: [],
  formatSubtitles: false, subtitleTrackSelections: [], subtitleFontFamily: "", subtitleFontSize: "48", subtitlePrimaryColor: "#FFFFFF", subtitleOutlineColor: "#000000", subtitleOutlineSize: "2", subtitleFontFileName: "", subtitleFontDataBase64: "", subtitleTimingOffsetMs: 0,
  muxMatchingExternalSubtitles: false, manualSubtitleSelections: [], externalSubtitleLanguage: "und", externalSubtitleTrackName: null, externalSubtitleFormats: "", preserveExternalSubtitleFiles: true, skipMuxIfSubtitleAlreadyExists: false,
  extractSubtitles: false, extractSubtitleLanguages: "", extractOverwriteExistingFiles: false, convertMp4ToMkv: false, deleteMp4AfterConvert: false
};

export function buildMuxRequest(files: MediaFileRow[], selectedPaths: string[], options: Partial<MuxOptions>): MuxPreviewRequest {
  return { files, selectedPaths, ...muxDefaults, ...options };
}

/** The shared operation flow over mkvmerge plans. */
export function useMuxRequest(options: Omit<OperationRunOptions<MuxPreviewRequest, MuxPreviewResponse>, "preview" | "apply" | "resultOf">) {
  return useOperationRun<MuxPreviewRequest, MuxPreviewResponse>({ ...options, preview: buildMuxPreview, apply: startMuxApply, resultOf: (job) => job.muxResult });
}
