import { expect, it } from "vitest";

import type { MuxPreviewResponse } from "../api";
import { buildMuxRequest } from "./useMuxRequest";
import { originalsAtRisk } from "./useConfirmedApply";

const action = (fileName: string, operation: string, description = "") => ({ index: 0, filePath: `/media/${fileName}`, fileName, operation, toolName: "mkvmerge", description, command: "" });
const plan = (...actions: ReturnType<typeof action>[]): MuxPreviewResponse => ({ actions, noChangeFiles: [], summary: "", status: "", planId: null, planFingerprint: null, idempotencyKey: null });
const none = { replaced: [], deletedMp4: [], deletedSubtitles: [], overwrittenExtracts: [] };

it("names the files a remux would replace", () => {
  expect(originalsAtRisk(buildMuxRequest([], [], {}), plan(action("A.mkv", "Remux"), action("A.mkv", "Remux"), action("B.mkv", "Mux subtitles")))).toEqual({
    ...none, replaced: [{ path: "/media/A.mkv", name: "A.mkv" }, { path: "/media/B.mkv", name: "B.mkv" }]
  });
});

it("keeps quiet when the originals are kept or only read", () => {
  expect(originalsAtRisk(buildMuxRequest([], [], { preserveOriginal: true }), plan(action("A.mkv", "Remux")))).toBeNull();
  expect(originalsAtRisk(buildMuxRequest([], [], { extractSubtitles: true }), plan(action("A.mkv", "Extract subtitles")))).toBeNull();
  expect(originalsAtRisk(buildMuxRequest([], [], { convertMp4ToMkv: true }), plan(action("A.mp4", "Convert to MKV")))).toBeNull();
});

it("names the MP4s deleted after conversion", () => {
  expect(originalsAtRisk(buildMuxRequest([], [], { convertMp4ToMkv: true, deleteMp4AfterConvert: true }), plan(action("A.mp4", "Convert to MKV")))?.deletedMp4).toEqual([{ path: "/media/A.mp4", name: "A.mp4" }]);
});

it("names the MKVs whose earlier extractions an overwriting extract replaces", () => {
  const risk = originalsAtRisk(buildMuxRequest([], [], { extractSubtitles: true, extractOverwriteExistingFiles: true }), plan(action("A.mkv", "Extract subtitles", "Extract 2 subtitle track(s)")));
  expect(risk).toEqual({ ...none, overwrittenExtracts: [{ path: "/media/A.mkv", name: "A.mkv" }] });
});

it("names the subtitle files a mux deletes, by path when they were picked by hand", () => {
  const request = buildMuxRequest([], [], {
    muxMatchingExternalSubtitles: true, preserveExternalSubtitleFiles: false,
    manualSubtitleSelections: [{ targetPath: "/media/A.mkv", subtitlePath: "/subs/A.forced.ass" }]
  });
  const description = "Remux with 2 matching external subtitle(s):\n• A.eng.srt (eng · English · default)\n• A.forced.ass (jpn · forced)";
  expect(originalsAtRisk(request, plan(action("A.mkv", "Mux subtitles", description)))).toEqual({
    ...none,
    replaced: [{ path: "/media/A.mkv", name: "A.mkv" }],
    deletedSubtitles: [{ path: "/media/A.mkv|A.eng.srt", name: "A.eng.srt" }, { path: "/subs/A.forced.ass", name: "/subs/A.forced.ass" }]
  });
});

it("falls back to the MKV when the plan does not list the subtitle files", () => {
  const request = buildMuxRequest([], [], { preserveOriginal: true, muxMatchingExternalSubtitles: true, preserveExternalSubtitleFiles: false });
  expect(originalsAtRisk(request, plan(action("A.mkv", "Mux subtitles", "Remux")))?.deletedSubtitles).toEqual([{ path: "/media/A.mkv", name: "Muxed into A.mkv" }]);
});
