import { describe, expect, it } from "vitest";

import type { MediaFileRow } from "../api";
import { describeTrackTargets, getTemplateMismatchMessages, hasTemplateMismatch, normalizeCompareValue, normalizeTrackSignature } from "./templateMatch";

type Track = MediaFileRow["tracks"][number];

function tracks(): Track[] {
  return [
    { id: 0, trackNumber: 1, type: "video", codec: "HEVC", language: "und", name: "", channels: null, default: true, forced: false },
    { id: 1, trackNumber: 2, type: "audio", codec: "AAC", language: "cat", name: "Català", channels: 2, default: true, forced: false },
    { id: 2, trackNumber: 3, type: "subtitles", codec: "ASS", language: "cat", name: "Complets", channels: null, default: false, forced: false }
  ];
}

function file(name: string, patch: Partial<MediaFileRow> = {}): MediaFileRow {
  return {
    path: `/media/${name}`, fileName: name, extension: ".mkv", status: "Scanned", reader: "mkvmerge",
    codec: "HEVC", resolution: "1920x1080", bitDepth: "10", hdr: "None", videoSummary: "HEVC",
    audioSummary: "cat x1", subtitleSummary: "cat x1", attachmentSummary: "None", tracks: tracks(), attachments: [],
    ...patch
  };
}

function withTrack(index: number, patch: Partial<Track>) {
  return tracks().map((track, position) => position === index ? { ...track, ...patch } : track);
}

const template = file("01.mkv");

function labels(other: MediaFileRow) {
  return getTemplateMismatchMessages(other, template).map((difference) => difference.label);
}

describe("template comparison", () => {
  it("reports no difference without a template, for the template itself, or for an identical file", () => {
    expect(getTemplateMismatchMessages(file("02.mkv", { codec: "AVC" }), null)).toEqual([]);
    expect(getTemplateMismatchMessages(template, template)).toEqual([]);
    expect(hasTemplateMismatch(file("02.mkv"), template)).toBe(false);
  });

  it("ignores case, surrounding spaces, and empty versus missing values", () => {
    expect(hasTemplateMismatch(file("02.mkv", { codec: " hevc ", audioSummary: "CAT x1" }), template)).toBe(false);
    expect(hasTemplateMismatch(file("02.mkv", { bitDepth: "" }), file("01.mkv", { bitDepth: "None" }))).toBe(false);
    expect(normalizeCompareValue(null)).toBe("none");
  });

  it.each([
    ["Codec", { codec: "AVC" }, "AVC", "HEVC"],
    ["Resolution", { resolution: "1280x720" }, "1280x720", "1920x1080"],
    ["Bit depth", { bitDepth: "8" }, "8", "10"],
    ["Audio summary", { audioSummary: "jpn x1" }, "jpn x1", "cat x1"],
    ["Subtitle summary", { subtitleSummary: "" }, "None", "cat x1"]
  ] as Array<[string, Partial<MediaFileRow>, string, string]>)("flags a different %s", (label, patch, value, templateValue) => {
    expect(getTemplateMismatchMessages(file("02.mkv", patch), template)).toEqual([{ label, value, templateValue }]);
  });

  it.each([
    ["mkvmerge ID", { id: 5 }, ["Track position 2 mkvmerge ID"]],
    ["property number", { trackNumber: 9 }, ["Audio track · ID 1 property number"]],
    ["type", { type: "subtitles" }, ["Subtitles track · ID 1 type"]],
    ["codec", { codec: "AC-3" }, ["Audio track · ID 1 codec"]],
    ["language", { language: "jpn" }, ["Audio track · ID 1 language"]],
    ["name", { name: "Japonès" }, ["Audio track · ID 1 name"]],
    ["default flag", { default: false }, ["Audio track · ID 1 · Default track"]],
    ["forced flag", { forced: true }, ["Audio track · ID 1 · Forced track"]]
  ] as Array<[string, Partial<Track>, string[]]>)("flags a track with a different %s", (_name, patch, expected) => {
    const other = file("02.mkv", { tracks: withTrack(1, patch) });
    expect(labels(other)).toEqual(expected);
    expect(hasTemplateMismatch(other, template)).toBe(true);
  });

  it("does not compare the name of a video track", () => {
    expect(hasTemplateMismatch(file("02.mkv", { tracks: withTrack(0, { name: "Episode 02" }) }), template)).toBe(false);
  });

  it("flags missing and extra tracks by position", () => {
    expect(getTemplateMismatchMessages(file("02.mkv", { tracks: tracks().slice(0, 2) }), template))
      .toEqual([{ label: "Missing track · ID 2", value: "Missing", templateValue: "subtitles" }]);
    const extra: Track = { id: 3, trackNumber: 4, type: "audio", codec: "AAC", language: "jpn", name: "", channels: 2, default: false, forced: false };
    expect(getTemplateMismatchMessages(file("02.mkv", { tracks: [...tracks(), extra] }), template))
      .toEqual([{ label: "Extra track · ID 3", value: "audio", templateValue: "No corresponding track" }]);
  });

  it("flags the same tracks in a different order", () => {
    const [video, audio, subtitle] = tracks();
    expect(hasTemplateMismatch(file("02.mkv", { tracks: [video, subtitle, audio] }), template)).toBe(true);
  });

  it("builds a track signature that leaves out the video track name", () => {
    expect(normalizeTrackSignature(file("02.mkv", { tracks: withTrack(0, { name: "Episode 02" }) }))).toBe(normalizeTrackSignature(template));
    expect(normalizeTrackSignature(file("02.mkv", { tracks: withTrack(1, { name: "Japonès" }) }))).not.toBe(normalizeTrackSignature(template));
    expect(normalizeTrackSignature(template)).toBe("0|1|video|hevc|und|none|default|none;1|2|audio|aac|cat|català|default|none;2|3|subtitles|ass|cat|complets|none|none");
  });
});

describe("track targets", () => {
  it("names the track in the same position within its type", () => {
    const [video, audio, subtitle] = tracks();
    const japanese: Track = { ...audio, id: 1, language: "jpn", name: "Japonès" };
    const catalan: Track = { ...audio, id: 2, trackNumber: 3 };
    const other = file("02.mkv", { tracks: [video, japanese, catalan, { ...subtitle, id: 3, trackNumber: 4 }] });

    expect(describeTrackTargets(other, template, "audio", ["Audio 1", "Audio 2"])).toEqual([
      "Audio 1 → track ID 1 · jpn · AAC · Japonès (template: track ID 1 · cat · AAC · Català)",
      "Audio 2 → track ID 2 · cat · AAC · Català (template: no track)"
    ]);
    expect(describeTrackTargets(other, template, "subtitle", ["Subtitle 1", "Subtitle 2"])).toEqual([
      "Subtitle 1 → track ID 3 · cat · ASS · Complets",
      "Subtitle 2 → no subtitle track in that position"
    ]);
  });
});
