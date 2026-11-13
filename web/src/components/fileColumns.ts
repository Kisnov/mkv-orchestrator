import type { ReactNode } from "react";

import type { MediaFileRow } from "../api";
import type { TemplateDifference } from "../lib/templateMatch";

export type FileRow = { file: MediaFileRow; differences: TemplateDifference[]; status: string };

export type FileColumn = {
  key: string;
  label: string;
  sortValue: (row: FileRow) => string;
  /** Defaults to the sort value as text. */
  cell?: (row: FileRow) => ReactNode;
  /** Long values are cut and shown whole on hover. */
  truncate?: boolean;
};

/** Columns FileSelectionTable can show after the file name; `status` is the Dashboard's template status. */
export const fileColumns = {
  reader: { key: "reader", label: "Reader", sortValue: (row) => row.file.reader },
  codec: { key: "codec", label: "Codec", sortValue: (row) => row.file.codec || "Unknown" },
  resolution: { key: "resolution", label: "Resolution", sortValue: (row) => row.file.resolution || "Unknown" },
  audio: { key: "audio", label: "Audio", sortValue: (row) => row.file.audioSummary || "None", truncate: true },
  subtitles: { key: "subtitles", label: "Subtitles", sortValue: (row) => row.file.subtitleSummary || "None", truncate: true },
  status: { key: "status", label: "Status", sortValue: (row) => row.status }
} satisfies Record<string, FileColumn>;
