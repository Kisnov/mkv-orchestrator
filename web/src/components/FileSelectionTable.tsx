import { Fragment, useId, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { AlertTriangle, ChevronDown, ChevronRight } from "lucide-react";

import type { MediaFileRow } from "../api";
import { getTemplateMismatchMessages } from "../lib/templateMatch";
import type { FileSelection } from "../state/useFileSelection";
import { fileColumns, type FileColumn, type FileRow } from "./fileColumns";
import { FileName } from "./FileName";
import { SortableColumnHeader, type SortDirection } from "./SortableColumnHeader";

const defaultColumns: FileColumn[] = [fileColumns.codec, fileColumns.audio, fileColumns.subtitles, fileColumns.status];
const toolbarButton = "rounded-md border border-border px-3 py-1.5 hover:bg-selected";
const notice = "flex items-center gap-2 rounded-md border border-warning bg-input px-3 py-2 text-xs text-warning";
const moveKeys = ["ArrowDown", "ArrowUp", "Home", "End"];

export function FileSelectionTable({ title, files, templateFile, selection, columns = defaultColumns, label, warnDiffering = false, matchingAction = true, activePath, onActivate }: {
  title: string;
  files: MediaFileRow[];
  templateFile: MediaFileRow | null;
  selection: FileSelection;
  /** Columns after the file name. */
  columns?: FileColumn[];
  /** Accessible name of the scrolling list. */
  label?: string;
  /** Warn that position-based edits land on other tracks in files that differ from the template. */
  warnDiffering?: boolean;
  /** Offer "Only equal to template". */
  matchingAction?: boolean;
  /** The row whose details the page shows. */
  activePath?: string;
  /** Makes rows pickable by click and from the keyboard: arrows move, Space checks, Enter picks. */
  onActivate?: (path: string) => void;
}) {
  const detailsId = useId();
  const [sort, setSort] = useState<{ key: string; direction: SortDirection }>({ key: "file", direction: "asc" });
  const [filter, setFilter] = useState("");
  const [expandedPath, setExpandedPath] = useState("");
  const shiftOnLabel = useRef(false);
  const rowElements = useRef(new Map<string, HTMLTableRowElement>());

  const rows = useMemo(() => {
    const multiplier = sort.direction === "asc" ? 1 : -1;
    const column = columns.find((item) => item.key === sort.key);
    const value = (row: FileRow) => column ? column.sortValue(row) : row.file.fileName;
    const needle = filter.trim().toLowerCase();
    return files.map((file, index) => {
      const differences = getTemplateMismatchMessages(file, templateFile);
      const status = file.path === templateFile?.path ? "Template" : differences.length > 0 ? "Warning" : file.status;
      return { file, index, differences, status };
    }).filter((row) => !needle || row.file.fileName.toLowerCase().includes(needle)).sort((left, right) => {
      const compared = value(left).localeCompare(value(right), undefined, { numeric: true, sensitivity: "base" });
      return compared === 0 ? left.index - right.index : compared * multiplier;
    });
  }, [files, templateFile, sort, columns, filter]);
  const displayedPaths = rows.map((row) => row.file.path);
  const selected = new Set(selection.selectedPaths);
  const matching = new Set(selection.matchingPaths);
  const selectedDiffering = selection.selectedPaths.filter((path) => selection.differingPaths.has(path)).length;
  const unselectedDiffering = selection.differingPaths.size - selectedDiffering;
  // One row is in the tab order: the active one, or the first.
  const focusPath = activePath && displayedPaths.includes(activePath) ? activePath : displayedPaths[0];

  function onRowKeyDown(event: KeyboardEvent<HTMLTableRowElement>, path: string) {
    if (event.target !== event.currentTarget) return;
    const index = displayedPaths.indexOf(path);
    if (moveKeys.includes(event.key)) {
      event.preventDefault();
      const target = event.key === "Home" ? 0 : event.key === "End" ? displayedPaths.length - 1 : Math.max(0, Math.min(displayedPaths.length - 1, index + (event.key === "ArrowDown" ? 1 : -1)));
      onActivate?.(displayedPaths[target]);
      rowElements.current.get(displayedPaths[target])?.focus();
    } else if (event.key === " ") {
      event.preventDefault();
      selection.toggle(path, event.shiftKey, displayedPaths);
    } else if (event.key === "Enter") {
      onActivate?.(path);
    }
  }

  return (
    <>
      <div className="flex shrink-0 flex-wrap items-center justify-between gap-3">
        <div className="min-w-0">
          <h2 className="text-base font-semibold">{title}</h2>
          <div className="mt-1 text-xs text-muted">{selected.size} of {files.length} selected{filter.trim() ? <span> · {rows.length} shown</span> : null}</div>
        </div>
        <div className="flex flex-wrap items-center gap-2 text-xs">
          <input type="search" aria-label="Filter files by name" placeholder="Filter by name" value={filter} onChange={(event) => setFilter(event.target.value)} className="h-8 w-40 rounded-md border border-border bg-input px-2.5 text-xs text-text outline-none placeholder:text-subtle focus:border-accent" />
          <button type="button" className={toolbarButton} onClick={() => selection.setWithin(displayedPaths, displayedPaths)}>Select all</button>
          <button type="button" className={toolbarButton} onClick={() => selection.setWithin(displayedPaths, [])}>Select none</button>
          <button type="button" className={toolbarButton} onClick={() => selection.setWithin(displayedPaths, displayedPaths.filter((path) => !selected.has(path)))}>Invert</button>
          {matchingAction ? <button type="button" className={toolbarButton} onClick={() => selection.setWithin(displayedPaths, displayedPaths.filter((path) => matching.has(path)))}>Only equal to template</button> : null}
        </div>
      </div>
      {warnDiffering ? (
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
      ) : null}
      <div className="mt-3 min-h-0 flex-1 overflow-auto" aria-label={label}>
        <table className="file-info-table file-selection-table border-collapse text-left text-sm">
          <thead className="sticky top-0 bg-card text-xs text-text">
            <tr>
              {[{ key: "file", label: "File" }, ...columns].map(({ key, label: heading }) => (
                <SortableColumnHeader key={key} active={sort.key === key} direction={sort.direction} label={heading} onSort={() => setSort((current) => ({ key, direction: current.key === key && current.direction === "asc" ? "desc" : "asc" }))} />
              ))}
            </tr>
          </thead>
          <tbody>
            {rows.length === 0 && files.length > 0 ? (
              <tr><td colSpan={columns.length + 1} className="border-b border-border px-3 py-3 text-subtle">No file names contain “{filter.trim()}”.</td></tr>
            ) : null}
            {rows.map((row) => {
              const { file, index, differences, status } = row;
              const expanded = expandedPath === file.path;
              const active = onActivate !== undefined && file.path === activePath;
              const checkbox = (
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
              );
              return (
                <Fragment key={file.path}>
                  <tr
                    ref={(element) => { if (element) rowElements.current.set(file.path, element); else rowElements.current.delete(file.path); }}
                    tabIndex={onActivate ? (file.path === focusPath ? 0 : -1) : undefined}
                    aria-current={active ? "true" : undefined}
                    onClick={onActivate ? (event) => { if (!(event.target as Element).closest("input, button, label")) onActivate(file.path); } : undefined}
                    onKeyDown={onActivate ? (event) => onRowKeyDown(event, file.path) : undefined}
                    className={[
                      active ? "bg-selected" : "bg-card hover:bg-selected",
                      onActivate ? "cursor-pointer" : "",
                      status === "Template" ? "text-template" : status === "Warning" ? "text-warning" : "text-text"
                    ].join(" ")}
                  >
                    <td className="border-b border-border px-3 py-2">
                      {onActivate ? (
                        // The name picks the row here, so only the checkbox selects.
                        <div className="flex min-w-0 items-center gap-3">{checkbox}<FileName value={file.fileName} /></div>
                      ) : (
                        <label
                          className="flex min-w-0 cursor-pointer select-none items-center gap-3"
                          // Shift+click on the name would otherwise extend a text selection.
                          onMouseDown={(event) => { if (event.shiftKey) event.preventDefault(); }}
                          onClickCapture={(event) => { if (!(event.target instanceof HTMLInputElement)) shiftOnLabel.current = event.shiftKey; }}
                        >
                          {checkbox}
                          <FileName value={file.fileName} />
                        </label>
                      )}
                    </td>
                    {columns.map((column) => column.key === "status" ? (
                      <td key={column.key} className={["border-b border-border px-3 py-2", status === "Template" || status === "Warning" ? "" : "text-success"].join(" ")}>
                        {status === "Warning" ? (
                          <button type="button" aria-expanded={expanded} aria-controls={`${detailsId}-${index}`} onClick={() => setExpandedPath(expanded ? "" : file.path)} className="inline-flex items-center gap-1 rounded hover:underline">
                            {expanded ? <ChevronDown size={13} aria-hidden="true" /> : <ChevronRight size={13} aria-hidden="true" />}
                            Warning ({differences.length})
                          </button>
                        ) : status}
                      </td>
                    ) : (
                      <td key={column.key} className={["border-b border-border px-3 py-2", column.truncate ? "max-w-[15.625rem] truncate" : ""].join(" ")} title={column.truncate ? column.sortValue(row) : undefined}>
                        {column.cell ? column.cell(row) : column.sortValue(row)}
                      </td>
                    ))}
                  </tr>
                  {expanded ? (
                    <tr id={`${detailsId}-${index}`} className="file-selection-details bg-input text-xs">
                      <td colSpan={columns.length + 1} className="border-b border-border px-3 py-2">
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
