import { expect, it } from "vitest";

import type { MediaFileRow } from "../api";
import { scanStats } from "./scanStats";

function file(name: string, patch: Partial<MediaFileRow> = {}, languages: Array<[string, string]> = [["audio", "jpn"], ["subtitles", "eng"]]): MediaFileRow {
  return {
    path: `/media/${name}`, fileName: name, extension: name.slice(name.lastIndexOf(".")), status: "Scanned", reader: "mkvmerge", codec: "HEVC/H.265", resolution: "1920x1080", bitDepth: "10", hdr: "None",
    videoSummary: "", audioSummary: "", subtitleSummary: "", attachmentSummary: "", attachments: [],
    tracks: languages.map(([type, language], id) => ({ id, trackNumber: id + 1, type, codec: "", language, name: "", channels: null, default: false, forced: false })),
    ...patch
  };
}

it("counts containers, codecs and the files each language appears in", () => {
  const stats = scanStats([
    file("01.mkv", {}, [["audio", "jpn"], ["audio", "jpn"], ["audio", "eng"], ["subtitles", "eng"]]),
    file("02.mkv", { codec: "AVC/H.264", hdr: "HDR10" }),
    file("03.mp4", { resolution: "" }, [["audio", ""]]),
    file("04.mkv", { status: "Scan failed", codec: "" }, [])
  ]);

  expect(stats).toMatchObject({ total: 4, mkv: 3, mp4: 1, unreadable: 1, hdr: 1 });
  expect(stats.video).toEqual([{ label: "HEVC/H.265", count: 2 }, { label: "AVC/H.264", count: 1 }]);
  expect(stats.resolution).toEqual([{ label: "1920x1080", count: 2 }, { label: "Unknown", count: 1 }]);
  // Two Japanese tracks in one file are one file with Japanese audio.
  expect(stats.audio).toEqual([{ label: "jpn", count: 2 }, { label: "eng", count: 1 }, { label: "und", count: 1 }]);
  expect(stats.subtitles).toEqual([{ label: "eng", count: 2 }]);
});
