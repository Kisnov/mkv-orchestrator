import { Fragment, useId, useMemo, useRef, useState } from "react";
import { AlertTriangle, ChevronDown, ChevronRight } from "lucide-react";

import type { MediaFileRow } from "../api";
import { getTemplateMismatchMessages } from "../lib/templateMatch";
import type { FileSelection } from "../state/useFileSelection";
import { FileName } from "./FileName";
import { SortableColumnHeader, type SortDirection } from "./SortableColumnHeader";

type SortKey = "file" | "codec" | "audio" | "subtitles" | "status";

const columns: Array<[SortKey, string]> = [
  ["file", "File"], ["codec", "Codec"], ["audio", "Audio"], ["subtitles", "Subtitles"], ["status", "Status"]
];
const toolbarButton = "rounded-md border border-border px-3 py-1.5 hover:bg-selected";
const notice = "flex items-center gap-2 rounded-md border border-warning bg-input px-3 py-2 text-xs text-warning";

export function FileSelectionTable({ title, files, templateFile, selection }: {
  title: string;
  files: MediaFileRow[];
  templateFile: MediaFileRow | null;
  selection: FileSelection;
}) {
  const detailsId = useId();
  const [sort, setSort] = useState<{ key: SortKey; direction: SortDirection }>({ key: "file", direction: "asc" });
  const [expandedPath, setExpandedPath] = useState("");
  const shiftOnLabel = useRef(false);

  const rows = useMemo(() => {
    const multiplier = sort.direction === "asc" ? 1 : -1;
    return files.map((file, index) => {
      const differences = getTemplateMismatchMessages(file, templateFile);
      const status = file.path === templateFile?.path ? "Template" : differences.length > 0 ? "Warning" : file.status;
      return { file, index, differences, status };
    }).sort((left, right) => {
      const compared = sortValue(left, sort.key).localeCompare(sortValue(right, sort.key), undefined, { numeric: true, sensitivity: "base" });
      return compared === 0 ? left.index - right.index : compared * multiplier;
    });
  }, [files, templateFile, sort]);
  const displayedPaths = rows.map((row) => row.file.path);
  const selected = new Set(selection.selectedPaths);
  const selectedDiffering = selection.selectedPaths.filter((path) => selection.differingPaths.has(path)).length;
  const unselectedDiffering = selection.differingPaths.size - selectedDiffering;

  return (
    <>
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-base font-semibold">{title}</h2>
          <div className="mt-1 text-xs text-muted">{selected.size} of {files.length} selected</div>
        </div>
        <div className="flex flex-wrap gap-2 text-xs">
          <button type="button" className={toolbarButton} onClick={selection.selectAll}>Select all</button>
          <button type="button" className={toolbarButton} onClick={selection.selectNone}>Select none</button>
          <button type="button" className={toolbarButton} onClick={selection.invert}>Invert</button>
          <button type="button" className={toolbarButton} onClick={selection.selectMatching}>Only equal to template</button>
        </div>
      </div>
      <div aria-live="polite" className="mt-3 shrink-0 space-y-2 empty:hidden">
        {unselectedDiffering > 0 ? (
          <p className={notice}>
            <AlertTriangle size={14} aria-hidden="true" className="shrink-0" />
            {unselectedDiffering === 1 ? "1 file differs from the template and is not selected." : `${unselectedDiffering} files differ from the template and are not selected.`}
          </p>
        ) : null}
        {selectedDiffering > 0 ? (
          <p className={notice}>
            <AlertTriangle size={14} aria-hidden="true" className="shrink-0" />
            {selectedDiffering === 1 ? "1 selected file differs" : `${selectedDiffering} selected files differ`} from the template. Edits go to the track in the same position, which may be a different track there.
          </p>
        ) : null}
      </div>
      <div className="mt-3 min-h-0 flex-1 overflow-auto">
        <table className="file-info-table file-selection-table border-collapse text-left text-sm">
          <thead className="sticky top-0 bg-card text-xs text-text">
            <tr>
              {columns.map(([key, label]) => (
                <SortableColumnHeader key={key} active={sort.key === key} direction={sort.direction} label={label} onSort={() => setSort((current) => ({ key, direction: current.key === key && current.direction === "asc" ? "desc" : "asc" }))} />
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.map(({ file, index, differences, status }) => {
              const expanded = expandedPath === file.path;
              return (
                <Fragment key={file.path}>
                  <tr className={["bg-card hover:bg-selected", status === "Template" ? "text-template" : status === "Warning" ? "text-warning" : "text-text"].join(" ")}>
                    <td className="border-b border-border px-3 py-2">
                      <label
                        className="flex min-w-0 cursor-pointer select-none items-center gap-3"
                        // Shift+click on the name would otherwise extend a text selection.
                        onMouseDown={(event) => { if (event.shiftKey) event.preventDefault(); }}
                        onClickCapture={(event) => { if (!(event.target instanceof HTMLInputElement)) shiftOnLabel.current = event.shiftKey; }}
                      >
                        <input
                          type="checkbox"
                          aria-label={`Select ${file.fileName}`}
                          checked={selected.has(file.path)}
                          onChange={(event) => {
                            const shiftKey = shiftOnLabel.current || (event.nativeEvent as MouseEvent).shiftKey;
                            shiftOnLabel.current = false;
                            selection.toggle(file.path, shiftKey, displayedPaths);
                          }}
                        />
                        <FileName value={file.fileName} />
                      </label>
                    </td>
                    <td className="border-b border-border px-3 py-2">{file.codec || "Unknown"}</td>
                    <td className="max-w-[15.625rem] truncate border-b border-border px-3 py-2" title={file.audioSummary}>{file.audioSummary || "None"}</td>
                    <td className="max-w-[15.625rem] truncate border-b border-border px-3 py-2" title={file.subtitleSummary}>{file.subtitleSummary || "None"}</td>
                    <td className={["border-b border-border px-3 py-2", status === "Template" || status === "Warning" ? "" : "text-success"].join(" ")}>
                      {status === "Warning" ? (
                        <button type="button" aria-expanded={expanded} aria-controls={`${detailsId}-${index}`} onClick={() => setExpandedPath(expanded ? "" : file.path)} className="inline-flex items-center gap-1 rounded hover:underline">
                          {expanded ? <ChevronDown size={13} aria-hidden="true" /> : <ChevronRight size={13} aria-hidden="true" />}
                          Warning ({differences.length})
                        </button>
                      ) : status}
                    </td>
                  </tr>
                  {expanded ? (
                    <tr id={`${detailsId}-${index}`} className="file-selection-details bg-input text-xs">
                      <td colSpan={columns.length} className="border-b border-border px-3 py-2">
                        <div className="font-semibold text-warning">Differs from template</div>
                        <ul className="mt-1 space-y-1 text-muted">
                          {differences.map((difference, position) => (
                            <li key={`${difference.label}-${position}`}>
                              <span className="font-semibold text-text">{difference.label}:</span> {difference.value} <span className="text-subtle">(template: {difference.templateValue})</span>
                            </li>
                          ))}
                        </ul>
                      </td>
                    </tr>
                  ) : null}
                </Fragment>
              );
            })}
          </tbody>
        </table>
      </div>
    </>
  );
}

function sortValue(row: { file: MediaFileRow; status: string }, key: SortKey) {
  if (key === "file") return row.file.fileName;
  if (key === "codec") return row.file.codec || "Unknown";
  if (key === "audio") return row.file.audioSummary || "None";
  if (key === "subtitles") return row.file.subtitleSummary || "None";
  return row.status;
}
