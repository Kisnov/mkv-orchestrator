import { expect, it } from "vitest";

import type { MuxPreviewResponse } from "../api";
import { buildMuxRequest } from "./useMuxRequest";
import { originalsAtRisk } from "./useConfirmedApply";

const action = (fileName: string, operation: string) => ({ index: 0, filePath: `/media/${fileName}`, fileName, operation, toolName: "mkvmerge", description: "", command: "" });
const plan = (...actions: ReturnType<typeof action>[]): MuxPreviewResponse => ({ actions, noChangeFiles: [], summary: "", status: "", planId: null, planFingerprint: null, idempotencyKey: null });

it("names the files a remux would replace", () => {
  expect(originalsAtRisk(buildMuxRequest([], [], {}), plan(action("A.mkv", "Remux"), action("A.mkv", "Remux"), action("B.mkv", "Mux subtitles")))).toEqual({
    replaced: [{ path: "/media/A.mkv", name: "A.mkv" }, { path: "/media/B.mkv", name: "B.mkv" }], deletedMp4: [], deletedSubtitlesOf: []
  });
});

it("keeps quiet when the originals are kept or only read", () => {
  expect(originalsAtRisk(buildMuxRequest([], [], { preserveOriginal: true }), plan(action("A.mkv", "Remux")))).toBeNull();
  expect(originalsAtRisk(buildMuxRequest([], [], {}), plan(action("A.mkv", "Extract subtitles")))).toBeNull();
  expect(originalsAtRisk(buildMuxRequest([], [], { convertMp4ToMkv: true }), plan(action("A.mp4", "Convert to MKV")))).toBeNull();
});

it("names the MP4s and subtitle files deleted after success", () => {
  expect(originalsAtRisk(buildMuxRequest([], [], { convertMp4ToMkv: true, deleteMp4AfterConvert: true }), plan(action("A.mp4", "Convert to MKV")))?.deletedMp4).toEqual([{ path: "/media/A.mp4", name: "A.mp4" }]);
  const risk = originalsAtRisk(buildMuxRequest([], [], { preserveOriginal: true, muxMatchingExternalSubtitles: true, preserveExternalSubtitleFiles: false }), plan(action("A.mkv", "Mux subtitles")));
  expect(risk).toEqual({ replaced: [], deletedMp4: [], deletedSubtitlesOf: [{ path: "/media/A.mkv", name: "A.mkv" }] });
});
