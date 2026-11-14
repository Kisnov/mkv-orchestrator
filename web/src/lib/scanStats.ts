import type { MediaFileRow } from "../api";

export type Share = { label: string; count: number };

export type ScanStats = {
  total: number;
  mkv: number;
  mp4: number;
  unreadable: number;
  hdr: number;
  video: Share[];
  resolution: Share[];
  /** Files with at least one audio track in the language. */
  audio: Share[];
  /** Files with at least one subtitle track in the language. */
  subtitles: Share[];
};

function shares(counts: Map<string, number>): Share[] {
  return [...counts].map(([label, count]) => ({ label, count })).sort((left, right) => right.count - left.count || left.label.localeCompare(right.label));
}

function add(counts: Map<string, number>, label: string) {
  counts.set(label, (counts.get(label) ?? 0) + 1);
}

/** What the scanned files are made of, counted from the rows already in memory. */
export function scanStats(files: MediaFileRow[]): ScanStats {
  const video = new Map<string, number>();
  const resolution = new Map<string, number>();
  const audio = new Map<string, number>();
  const subtitles = new Map<string, number>();
  let mkv = 0, mp4 = 0, unreadable = 0, hdr = 0;
  for (const file of files) {
    const extension = file.extension.toLowerCase();
    if (extension === ".mkv") mkv += 1;
    if (extension === ".mp4") mp4 += 1;
    if (file.status.toLowerCase().includes("failed")) { unreadable += 1; continue; }
    if (file.hdr && file.hdr.toLowerCase() !== "none") hdr += 1;
    add(video, file.codec || "Unknown");
    add(resolution, file.resolution || "Unknown");
    const languages = (kind: string) => new Set(file.tracks.filter((track) => track.type.toLowerCase().startsWith(kind)).map((track) => track.language.trim() || "und"));
    languages("audio").forEach((language) => add(audio, language));
    languages("sub").forEach((language) => add(subtitles, language));
  }
  return { total: files.length, mkv, mp4, unreadable, hdr, video: shares(video), resolution: shares(resolution), audio: shares(audio), subtitles: shares(subtitles) };
}
