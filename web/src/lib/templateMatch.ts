import type { MediaFileRow } from "../api";

// Shared by the Dashboard status and the Edit Tracks default selection.

export function normalizeCompareValue(value: string | null | undefined) {
  const clean = (value ?? "").trim();
  return clean.length === 0 ? "none" : clean.toLowerCase();
}

export function hasTemplateMismatch(file: MediaFileRow, templateFile: MediaFileRow | null) {
  return getTemplateMismatchMessages(file, templateFile).length > 0;
}

export type TemplateDifference = { label: string; value: string; templateValue: string };

export function getTemplateMismatchMessages(file: MediaFileRow, templateFile: MediaFileRow | null) {
  if (!templateFile || file.path === templateFile.path) return [];

  const messages: TemplateDifference[] = [];
  addValueMismatch(messages, "Codec", file.codec, templateFile.codec);
  addValueMismatch(messages, "Resolution", file.resolution, templateFile.resolution);
  addValueMismatch(messages, "Bit depth", file.bitDepth, templateFile.bitDepth);
  addValueMismatch(messages, "Audio summary", file.audioSummary, templateFile.audioSummary);
  addValueMismatch(messages, "Subtitle summary", file.subtitleSummary, templateFile.subtitleSummary);

  const trackCount = Math.max(file.tracks.length, templateFile.tracks.length);
  for (let index = 0; index < trackCount; index += 1) {
    const track = file.tracks[index];
    const templateTrack = templateFile.tracks[index];

    if (!track && templateTrack) {
      messages.push({ label: `Missing track · ID ${templateTrack.id}`, value: "Missing", templateValue: formatCompareValue(templateTrack.type) });
      continue;
    }
    if (track && !templateTrack) {
      messages.push({ label: `Extra track · ID ${track.id}`, value: formatCompareValue(track.type), templateValue: "No corresponding track" });
      continue;
    }
    if (!track || !templateTrack) continue;

    const label = `${track.type.charAt(0).toUpperCase() + track.type.slice(1)} track · ID ${track.id}`;
    addValueMismatch(messages, `Track position ${index + 1} mkvmerge ID`, String(track.id), String(templateTrack.id));
    addValueMismatch(messages, `${label} property number`, String(track.trackNumber), String(templateTrack.trackNumber));
    addValueMismatch(messages, `${label} type`, track.type, templateTrack.type);
    addValueMismatch(messages, `${label} codec`, track.codec, templateTrack.codec);
    addValueMismatch(messages, `${label} language`, track.language, templateTrack.language);
    const isVideo = normalizeCompareValue(track.type) === "video" || normalizeCompareValue(templateTrack.type) === "video";
    if (!isVideo) addValueMismatch(messages, `${label} name`, track.name, templateTrack.name);
    addValueMismatch(messages, `${label} · Default track`, track.default ? "Yes" : "No", templateTrack.default ? "Yes" : "No");
    addValueMismatch(messages, `${label} · Forced track`, track.forced ? "Yes" : "No", templateTrack.forced ? "Yes" : "No");
  }

  return messages;
}

function addValueMismatch(messages: TemplateDifference[], label: string, value: string, templateValue: string) {
  if (normalizeCompareValue(value) === normalizeCompareValue(templateValue)) return;
  messages.push({ label, value: formatCompareValue(value), templateValue: formatCompareValue(templateValue) });
}

function formatCompareValue(value: string | null | undefined) {
  const clean = (value ?? "").trim();
  return clean || "None";
}

export function normalizeTrackSignature(file: MediaFileRow) {
  return file.tracks
    .map((track) => {
      const isVideo = normalizeCompareValue(track.type) === "video";
      return [
        String(track.id),
        String(track.trackNumber),
        track.type,
        track.codec,
        track.language,
        isVideo ? "" : track.name,
        track.default ? "default" : "",
        track.forced ? "forced" : ""
      ].map(normalizeCompareValue).join("|");
    })
    .join(";");
}

type TrackRow = MediaFileRow["tracks"][number];

/** One line per template track naming the track of `file` that an edit by position lands on. */
export function describeTrackTargets(file: MediaFileRow, templateFile: MediaFileRow, kind: "audio" | "subtitle", labels: string[]) {
  const ofKind = (row: MediaFileRow) => row.tracks.filter((track) => normalizeCompareValue(track.type).startsWith(kind));
  const own = ofKind(file);
  const reference = ofKind(templateFile);
  return labels.map((label, index) => {
    const track = own[index];
    const templateTrack = reference[index];
    if (!track) return `${label} → no ${kind} track in that position`;
    const same = templateTrack && (["codec", "language", "name"] as const).every((key) => normalizeCompareValue(track[key]) === normalizeCompareValue(templateTrack[key]));
    return `${label} → ${describeTrack(track)}${same ? "" : ` (template: ${templateTrack ? describeTrack(templateTrack) : "no track"})`}`;
  });
}

function describeTrack(track: TrackRow) {
  return `track ID ${track.id} · ${track.language || "und"} · ${track.codec || "Unknown"} · ${track.name || "No name"}`;
}
