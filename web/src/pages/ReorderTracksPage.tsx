import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ArrowDown, ArrowUp } from "lucide-react";

import { getCurrentScanFiles } from "../api";
import { fileColumns } from "../components/fileColumns";
import { FileSelectionTable } from "../components/FileSelectionTable";
import { OperationShell } from "../components/OperationShell";
import { OriginalsConfirm } from "../components/OriginalsConfirm";
import { pathKey } from "../lib/pathKey";
import { normalizeTrackSignature } from "../lib/templateMatch";
import { useMediaLibrary } from "../state/MediaLibraryContext";
import { useLibrarySelection } from "../state/useFileSelection";
import { useConfirmedApply } from "../state/useConfirmedApply";
import { buildMuxRequest, useMuxRequest } from "../state/useMuxRequest";

const columns = [fileColumns.reader, fileColumns.codec, fileColumns.audio, fileColumns.subtitles, fileColumns.status];

export function ReorderTracksPage() {
  const { files, setSelectedPaths, templateFilePath, syncFromBackend, isWorkingView } = useMediaLibrary();
  const currentScan = useQuery({ queryKey: ["current-scan-files"], queryFn: getCurrentScanFiles });
  const mkvFiles = useMemo(() => files.filter((file) => file.extension.toLowerCase() === ".mkv"), [files]);
  const initializedSelectionScope = useRef("");
  const [templatePath, setTemplatePath] = useState("");
  const [order, setOrder] = useState<number[]>([]);
  const run = useMuxRequest({
    label: "Reorder Tracks",
    initialStatus: "Select batch files, arrange the template tracks, then preview the changes.",
    startedText: (total) => `Reordering tracks in ${total} file(s)...`,
    noChangesText: "No track-order changes are needed for the selected files."
  });
  const confirmed = useConfirmedApply(run);

  useEffect(() => {
    if (!currentScan.data) return;
    const scan = currentScan.data;
    const scope = scan.updatedUtc ?? scan.files.map((file) => file.path).join("|");
    syncFromBackend(scan);
    if (scan.files.length === 0) {
      run.setPreviewResult(null);
    } else if (initializedSelectionScope.current !== scope) {
      const opening = initializedSelectionScope.current === "";
      initializedSelectionScope.current = scope;
      const mkvPaths = (isWorkingView ? files : scan.files).filter((file) => file.extension.toLowerCase() === ".mkv").map((file) => file.path);
      const known = new Set(mkvPaths.map(pathKey));
      // Opening the page starts a full batch; a later scan keeps the selection unless none of it is left.
      if (opening || !scan.selectedPaths.some((path) => known.has(pathKey(path)))) setSelectedPaths(mkvPaths);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentScan.data, isWorkingView]);

  useEffect(() => {
    const preferred = mkvFiles.find((file) => pathKey(file.path) === pathKey(templateFilePath))?.path;
    setTemplatePath((current) => mkvFiles.some((file) => pathKey(file.path) === pathKey(current)) ? current : preferred ?? mkvFiles[0]?.path ?? "");
  }, [mkvFiles, templateFilePath]);

  const template = useMemo(() => mkvFiles.find((file) => pathKey(file.path) === pathKey(templatePath)) ?? null, [mkvFiles, templatePath]);
  const selection = useLibrarySelection(mkvFiles, template);
  const selectedMkvPaths = selection.selectedPaths;

  // An apply renumbers the template's tracks under the same path, so the layout resets the order too.
  const templateLayout = template ? normalizeTrackSignature(template) : "";
  useEffect(() => {
    setOrder(template?.tracks.slice().sort((left, right) => left.trackNumber - right.trackNumber).map((track) => track.id) ?? []);
    run.setPreviewResult(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [template?.path, templateLayout]);

  const orderedTracks = order
    .map((id) => template?.tracks.find((track) => track.id === id))
    .filter(Boolean) as NonNullable<typeof template>["tracks"];

  function chooseTemplate(path: string) {
    setTemplatePath(path);
    if (!selectedMkvPaths.includes(path)) selection.setWithin([path], [path]);
  }

  function move(index: number, delta: number) {
    const target = index + delta;
    if (target < 0 || target >= order.length) return;
    setOrder((current) => { const next = [...current]; [next[index], next[target]] = [next[target], next[index]]; return next; });
    run.setPreviewResult(null);
  }

  const request = () => buildMuxRequest(files, selectedMkvPaths, { reorderTracks: true, reorderTemplatePath: templatePath, reorderTemplateTrackIds: order });
  const canRun = Boolean(template && selectedMkvPaths.some((path) => pathKey(path) === pathKey(template.path)));
  const previewResult = run.previewResult;

  return (
    <OperationShell
      title="Reorder Tracks"
      description="Use one template to reorder tracks consistently across all selected MKV files."
      run={run}
      canRun={canRun}
      onPreview={() => { if (!canRun) { run.setStatus("Select a template file and include it in the batch."); return; } run.preview(request()); }}
      onApply={() => { if (canRun) void confirmed.apply(request()); }}
      selectedText={`${selectedMkvPaths.length} files selected`}
      hiddenSelected={selectedMkvPaths.filter(selection.isHidden).length}
      risks={["Replaces originals"]}
      notice={template && !canRun ? <span className="w-full text-xs text-warning">The template file is not in the batch. Check it in the list to continue.</span> : null}
      options={<div className="space-y-3">
        <h2 className="text-base font-semibold">Reorder Options</h2>
        <label className="block text-sm font-semibold" htmlFor="reorder-template">Template file</label>
        <select id="reorder-template" className="h-9 w-full rounded-md border border-border bg-input px-2 text-sm" value={templatePath} onChange={(event) => chooseTemplate(event.target.value)}>{mkvFiles.map((file) => <option key={file.path} value={file.path}>{file.fileName}</option>)}</select>
        <p className="text-xs leading-5 text-muted">Arrange the template below. Every checked file will be matched by track type, language, name, codec, and audio channels.</p>
        <div className="rounded-md border border-border bg-panel p-3 text-xs leading-5 text-muted">The original files are replaced using validated temporary files with automatic rollback. Files that cannot be matched safely are blocked.</div>
      </div>}
      files={<FileSelectionTable title="File Info" files={mkvFiles} templateFile={template} selection={selection} columns={columns} label="Reorder Tracks file selection" />}
      details={<>
        <div className="flex shrink-0 items-center justify-between gap-3"><h2 className="text-sm font-semibold">Template Track Order</h2><span className="truncate text-xs text-muted">{template?.fileName ?? "No template"}</span></div>
        <div className="mt-3 min-h-0 flex-1 overflow-auto"><table className="w-full min-w-[45rem] table-fixed border-collapse text-left text-sm">
          <thead className="sticky top-0 bg-card text-xs text-text"><tr><th className="w-16 border-b border-border px-3 py-2">Order</th><th className="w-24 border-b border-border px-3 py-2">Type</th><th className="w-20 border-b border-border px-3 py-2">Lang</th><th className="w-40 border-b border-border px-3 py-2">Codec</th><th className="border-b border-border px-3 py-2">Name</th><th className="w-24 border-b border-border px-3 py-2">Move</th></tr></thead>
          <tbody>{orderedTracks.map((track: NonNullable<typeof template>["tracks"][number], index: number) => <tr key={track.id} className="bg-card hover:bg-selected"><td className="border-b border-border px-3 py-2 font-mono text-accent">#{index + 1}</td><td className="border-b border-border px-3 py-2">{track.type}</td><td className="border-b border-border px-3 py-2">{track.language || "und"}</td><td className="truncate border-b border-border px-3 py-2">{track.codec || "Unknown"}</td><td className="truncate border-b border-border px-3 py-2">{track.name || "-"}</td><td className="border-b border-border px-3 py-2"><div className="flex gap-1"><button aria-label={`Move ${track.name || `track ${track.id}`} up`} disabled={index === 0} onClick={() => move(index, -1)} className="rounded border border-border p-1 disabled:text-disabled"><ArrowUp size={14}/></button><button aria-label={`Move ${track.name || `track ${track.id}`} down`} disabled={index === orderedTracks.length - 1} onClick={() => move(index, 1)} className="rounded border border-border p-1 disabled:text-disabled"><ArrowDown size={14}/></button></div></td></tr>)}</tbody>
        </table>{!template ? <div className="flex h-full items-center justify-center text-sm text-muted">Load MKV files from Dashboard or Library first.</div> : null}</div>
      </>}
      summary={{ title: "Reorder Tracks Preview Summary", emptyText: "Build a preview to see track-order changes.", available: previewResult !== null, status: previewResult?.status ?? "", summary: previewResult?.summary ?? "", metrics: [{ label: "Files changing", value: new Set(previewResult?.actions.map((action) => action.filePath) ?? []).size, tone: "text-success" }, { label: "No change", value: previewResult?.noChangeFiles.length ?? 0, tone: "text-muted" }], sections: [{ title: "Planned operations", emptyText: "No track-order changes are needed.", rows: previewResult?.actions.map((action) => ({ key: `${action.filePath}-${action.index}`, title: action.fileName, detail: action.description })) ?? [] }] }}
    >
      {confirmed.dialog ? <OriginalsConfirm {...confirmed.dialog} /> : null}
    </OperationShell>
  );
}
