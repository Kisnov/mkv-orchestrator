import { useEffect, useMemo, useRef, useState } from "react";

import { getWebSettings, saveWebSettings } from "../api";
import type { FileColumn } from "../components/fileColumns";
import { FileSelectionTable } from "../components/FileSelectionTable";
import { OperationShell } from "../components/OperationShell";
import { OriginalsConfirm } from "../components/OriginalsConfirm";
import { pathKey } from "../lib/pathKey";
import { useMediaLibrary } from "../state/MediaLibraryContext";
import { useLibrarySelection } from "../state/useFileSelection";
import { useConfirmedApply } from "../state/useConfirmedApply";
import { buildMuxRequest, useMuxRequest } from "../state/useMuxRequest";
import { builtInSubtitleFonts, deleteImportedFont, fileToSubtitleFont, loadImportedFonts, registerPreviewFont, resolveFontData, type SubtitleFont } from "../lib/subtitleFonts";

export function SubtitleFormatPage() {
  const { files } = useMediaLibrary();
  const mkvFiles = useMemo(() => files.filter((file) => file.extension.toLowerCase() === ".mkv"), [files]);
  const selection = useLibrarySelection(mkvFiles, null);
  const selectedMkvPaths = selection.selectedPaths;
  const [fontFamily, setFontFamily] = useState("Andika Basic");
  const [fontSize, setFontSize] = useState("48");
  const [fontLibrary, setFontLibrary] = useState<SubtitleFont[]>(builtInSubtitleFonts);
  const [selectedFontId, setSelectedFontId] = useState(builtInSubtitleFonts[0].id);
  const [fontFileName, setFontFileName] = useState("");
  const [fontDataBase64, setFontDataBase64] = useState("");
  const [primaryColor, setPrimaryColor] = useState("#FFFFFF");
  const [outlineColor, setOutlineColor] = useState("#000000");
  const [outlineSize, setOutlineSize] = useState("2");
  const [keepTiming, setKeepTiming] = useState(true);
  const [timingOffset, setTimingOffset] = useState("0");
  const [selectedTrackIds, setSelectedTrackIds] = useState<Record<string, number[]>>({});
  const run = useMuxRequest({
    label: "Subtitle Format",
    initialStatus: "Select MKV files and choose the new subtitle style. SRT tracks will be converted to ASS.",
    startedText: (total) => `Formatting subtitles in ${total} file(s)...`,
    noChangesText: "No compatible subtitle changes are available."
  });
  const { setStatus, setPreviewResult, previewResult } = run;
  const confirmed = useConfirmedApply(run);
  const offsetMs = keepTiming ? 0 : Math.round((Number(timingOffset) || 0) * 1000);
  const selectedTrackCount = selectedMkvPaths.reduce((count, path) => count + (selectedTrackIds[pathKey(path)]?.length ?? 0), 0);
  const compatibleCount = selectedMkvPaths.filter((path) => (selectedTrackIds[pathKey(path)]?.length ?? 0) > 0).length;
  const batchTrackIds = useMemo(() => {
    const counts = new Map<number, { available: number; selected: number }>();
    const chosenFiles = new Set(selectedMkvPaths);
    for (const file of mkvFiles.filter((candidate) => chosenFiles.has(candidate.path))) {
      const selected = selectedTrackIds[pathKey(file.path)] ?? [];
      for (const track of file.tracks.filter(isEditableSubtitle)) {
        const count = counts.get(track.id) ?? { available: 0, selected: 0 };
        count.available += 1;
        if (selected.includes(track.id)) count.selected += 1;
        counts.set(track.id, count);
      }
    }
    return [...counts.entries()].sort(([left], [right]) => left - right).map(([id, count]) => ({ id, ...count }));
  }, [mkvFiles, selectedMkvPaths, selectedTrackIds]);

  useEffect(() => {
    void loadSavedFonts();
    void selectFont(builtInSubtitleFonts[0]);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  async function loadSavedFonts() {
    try {
      const settings = await getWebSettings();
      const serverFonts = (settings.subtitleFonts ?? []).map((font) => ({ ...font, builtIn: false } satisfies SubtitleFont));
      const legacyFonts = await loadImportedFonts().catch(() => []);
      const imported = uniqueFonts([...serverFonts, ...legacyFonts]);
      setFontLibrary([...builtInSubtitleFonts, ...imported]);
      if (legacyFonts.some((font) => !serverFonts.some((saved) => saved.id === font.id))) {
        await saveWebSettings({ subtitleFonts: imported.map(fontDto) });
      }
    } catch (error) {
      setStatus(error instanceof Error ? `Saved fonts could not be loaded: ${error.message}` : "Saved fonts could not be loaded from MKVO storage.");
    }
  }

  useEffect(() => {
    setSelectedTrackIds((current) => {
      const next = { ...current };
      for (const file of mkvFiles) {
        const key = pathKey(file.path);
        if (!(key in next)) next[key] = file.tracks.filter(isEditableSubtitle).map((track) => track.id);
      }
      return next;
    });
  }, [mkvFiles]);

  async function selectFont(font: SubtitleFont): Promise<boolean> {
    try {
      const resolved = await resolveFontData(font);
      setSelectedFontId(font.id); setFontFamily(font.family); setFontFileName(font.fileName); setFontDataBase64(resolved.dataBase64 ?? ""); setPreviewResult(null);
      try {
        await registerPreviewFont(resolved);
        return true;
      } catch {
        setStatus(`${font.family} is selected, but this browser cannot render a live preview of the font. It can still be attached and used for subtitle formatting.`);
        return false;
      }
    } catch (error) { setStatus(error instanceof Error ? error.message : "The font could not be loaded."); return false; }
  }

  async function importFont(file: File | undefined) {
    if (!file) return;
    if (!/\.(?:ttf|otf)$/i.test(file.name)) { setStatus("Choose a TrueType (.ttf) or OpenType (.otf) font."); return; }
    if (file.size > 1024 * 1024) { setStatus("Font files must be 1 MB or smaller."); return; }
    try {
      const font = await fileToSubtitleFont(file);
      setFontLibrary((current) => [...current.filter((item) => item.id !== font.id), font]);
      const previewAvailable = await selectFont(font);
      try {
        const imported = uniqueFonts([...fontLibrary.filter((item) => !item.builtIn), font]);
        await saveWebSettings({ subtitleFonts: imported.map(fontDto) });
        setStatus(`${font.family} was imported and saved for future operations.${previewAvailable ? "" : " Live preview is unavailable for this font, but subtitle formatting can still use it."}`);
      } catch {
        setStatus(`${font.family} was imported for this session, but MKVO storage could not save it. It will need to be imported again after a restart.`);
      }
    } catch (error) { setStatus(error instanceof Error ? error.message : "The font could not be imported."); }
  }

  async function removeSelectedFont() {
    const selected = fontLibrary.find((font) => font.id === selectedFontId);
    if (!selected || selected.builtIn) return;
    try {
      const remaining = fontLibrary.filter((font) => !font.builtIn && font.id !== selected.id);
      await saveWebSettings({ subtitleFonts: remaining.map(fontDto) });
      await deleteImportedFont(selected.id);
      setFontLibrary((current) => current.filter((font) => font.id !== selected.id));
      await selectFont(builtInSubtitleFonts[0]);
      setStatus(`${selected.family} was removed from saved fonts.`);
    } catch (error) { setStatus(error instanceof Error ? error.message : "The imported font could not be removed."); }
  }

  const request = () => buildMuxRequest(files, selectedMkvPaths, {
    formatSubtitles: true, subtitleTrackSelections: selectedMkvPaths.map((filePath) => ({ filePath, trackIds: selectedTrackIds[pathKey(filePath)] ?? [] })), subtitleFontFamily: fontFamily.trim(), subtitleFontSize: fontSize, subtitlePrimaryColor: primaryColor, subtitleOutlineColor: outlineColor, subtitleOutlineSize: outlineSize, subtitleFontFileName: fontFileName, subtitleFontDataBase64: fontDataBase64, subtitleTimingOffsetMs: offsetMs
  });
  const canRun = Boolean(fontFamily.trim()) && selectedMkvPaths.length > 0 && selectedTrackCount > 0;
  const shiftedTime = formatOffset(offsetMs);

  function toggleTrack(filePath: string, trackId: number) {
    const key = pathKey(filePath);
    setSelectedTrackIds((current) => {
      const selected = current[key] ?? [];
      return { ...current, [key]: selected.includes(trackId) ? selected.filter((id) => id !== trackId) : [...selected, trackId] };
    });
    setPreviewResult(null);
  }

  function setBatchTrack(trackId: number, select: boolean) {
    const chosenFiles = new Set(selectedMkvPaths);
    setSelectedTrackIds((current) => {
      const next = { ...current };
      for (const file of mkvFiles) {
        if (!chosenFiles.has(file.path) || !file.tracks.some((track) => track.id === trackId && isEditableSubtitle(track))) continue;
        const key = pathKey(file.path);
        const selected = current[key] ?? [];
        next[key] = select ? [...new Set([...selected, trackId])] : selected.filter((id) => id !== trackId);
      }
      return next;
    });
    setPreviewResult(null);
  }

  function setAllBatchTracks(select: boolean) {
    const chosenFiles = new Set(selectedMkvPaths);
    setSelectedTrackIds((current) => {
      const next = { ...current };
      for (const file of mkvFiles) {
        if (!chosenFiles.has(file.path)) continue;
        next[pathKey(file.path)] = select ? file.tracks.filter(isEditableSubtitle).map((track) => track.id) : [];
      }
      return next;
    });
    setPreviewResult(null);
  }

  const chosenFiles = new Set(selectedMkvPaths);
  const columns: FileColumn[] = [
    {
      key: "tracks", label: "Tracks to format",
      sortValue: ({ file }) => file.tracks.filter(isEditableSubtitle).map((track) => track.id).join(" "),
      cell: ({ file }) => {
        const editableTracks = file.tracks.filter(isEditableSubtitle);
        const selected = chosenFiles.has(file.path);
        const chosenIds = selectedTrackIds[pathKey(file.path)] ?? [];
        return editableTracks.length ? <div className="flex flex-wrap gap-x-4 gap-y-2">{editableTracks.map((track) => <label key={track.id} className={["flex items-center gap-2", selected ? "" : "text-disabled"].join(" ")}><input aria-label={`${file.fileName} track ${track.trackNumber} ${track.name || track.language || "Subtitle"}`} type="checkbox" checked={chosenIds.includes(track.id)} disabled={!selected} onChange={() => toggleTrack(file.path, track.id)} /><span>ID {track.id} (#{track.trackNumber}) · {track.name || "Subtitle"} · {track.language || "und"}{isSrtSubtitle(track) ? " · SRT → ASS" : ""}</span></label>)}</div> : <span className="text-muted">No ASS/SSA/SRT tracks</span>;
      }
    },
    {
      key: "format-status", label: "Status",
      sortValue: ({ file }) => formatStatus(file.tracks.filter(isEditableSubtitle).length, selectedTrackIds[pathKey(file.path)]?.length ?? 0),
      cell: ({ file }) => {
        const editable = file.tracks.filter(isEditableSubtitle).length;
        const chosen = selectedTrackIds[pathKey(file.path)]?.length ?? 0;
        return <span className={editable && chosen ? "text-success" : "text-warning"}>{formatStatus(editable, chosen)}</span>;
      }
    }
  ];

  return (
    <OperationShell
      title="Subtitle Format"
      description="Batch-style ASS/SSA subtitles or convert SRT tracks to styled ASS while preserving timing."
      run={run}
      canRun={canRun}
      onPreview={() => { if (canRun) run.preview(request()); }}
      onApply={() => { if (canRun) void confirmed.apply(request()); }}
      selectedText={`${selectedMkvPaths.length} files selected · ${selectedTrackCount} tracks across ${compatibleCount} files`}
      hiddenSelected={selectedMkvPaths.filter(selection.isHidden).length}
      options={<div className="space-y-3">
        <h2 className="text-base font-semibold">Format Options</h2>
        <label className="block"><span className="text-xs font-semibold text-muted">Saved font</span><select aria-label="Saved font" value={selectedFontId} onChange={(event) => { const font = fontLibrary.find((item) => item.id === event.target.value); if (font) void selectFont(font); }} className="mt-1.5 h-9 w-full rounded-md border border-border bg-input px-2 text-sm"><optgroup label="Bundled fonts">{fontLibrary.filter((font) => font.builtIn).map((font) => <option key={font.id} value={font.id}>{font.family}</option>)}</optgroup>{fontLibrary.some((font) => !font.builtIn) ? <optgroup label="Imported fonts">{fontLibrary.filter((font) => !font.builtIn).map((font) => <option key={font.id} value={font.id}>{font.family}</option>)}</optgroup> : null}</select></label>
        <div className="flex gap-2"><label className="flex h-9 flex-1 cursor-pointer items-center justify-center rounded-md border border-border bg-button px-2 text-xs font-semibold">Import .ttf/.otf<input aria-label="Import font file" type="file" accept=".ttf,.otf,font/ttf,font/otf" className="sr-only" onChange={(event) => { void importFont(event.target.files?.[0]); event.target.value = ""; }} /></label><button type="button" onClick={() => void removeSelectedFont()} disabled={fontLibrary.find((font) => font.id === selectedFontId)?.builtIn !== false} className="h-9 rounded-md border border-border px-2 text-xs disabled:text-disabled">Remove imported</button></div>
        <label className="block"><span className="text-xs font-semibold text-muted">ASS font family name</span><input aria-label="New font family" value={fontFamily} onChange={(event) => { setFontFamily(event.target.value); setPreviewResult(null); }} className="mt-1.5 h-9 w-full rounded-md border border-border bg-input px-3 text-sm" placeholder="Font family" /></label>
        <label className="block"><span className="text-xs font-semibold text-muted">Font size</span><input aria-label="Font size" type="number" min="1" max="500" step="0.5" value={fontSize} onChange={(event) => { setFontSize(event.target.value); setPreviewResult(null); }} className="mt-1.5 h-9 w-full rounded-md border border-border bg-input px-3 text-sm" /></label>
        <div className="grid grid-cols-2 gap-2"><ColorField label="Font color" value={primaryColor} onChange={(value) => { setPrimaryColor(value); setPreviewResult(null); }} /><ColorField label="Outline color" value={outlineColor} onChange={(value) => { setOutlineColor(value); setPreviewResult(null); }} /></div>
        <label className="block"><span className="text-xs font-semibold text-muted">Outline thickness</span><input aria-label="Outline thickness" type="number" min="0" max="20" step="0.5" value={outlineSize} onChange={(event) => { setOutlineSize(event.target.value); setPreviewResult(null); }} className="mt-1.5 h-9 w-full rounded-md border border-border bg-input px-3 text-sm" /></label>
        <fieldset className="space-y-2"><legend className="mb-2 text-xs font-semibold text-muted">Subtitle timing</legend>
          <label className="flex items-center gap-2 text-sm"><input type="radio" name="subtitle-timing" checked={keepTiming} onChange={() => { setKeepTiming(true); setPreviewResult(null); }} /> Keep the same timing</label>
          <label className="flex items-center gap-2 text-sm"><input type="radio" name="subtitle-timing" checked={!keepTiming} onChange={() => { setKeepTiming(false); setPreviewResult(null); }} /> Shift all cues</label>
        </fieldset>
        {!keepTiming ? <label className="block"><span className="text-xs font-semibold text-muted">Offset in seconds</span><input aria-label="Timing offset in seconds" type="number" step="0.1" value={timingOffset} onChange={(event) => { setTimingOffset(event.target.value); setPreviewResult(null); }} className="mt-1.5 h-9 w-full rounded-md border border-border bg-input px-3 text-sm" /><span className="mt-1 block text-xs text-muted">Positive delays subtitles; negative shows them earlier.</span></label> : null}
        <div className="rounded-md border border-border bg-panel p-3"><div className="flex items-center justify-between gap-2"><h3 className="text-xs font-semibold text-muted">Batch subtitle Track IDs</h3><div className="flex gap-1"><button type="button" onClick={() => setAllBatchTracks(true)} disabled={!batchTrackIds.length} className="rounded border border-border px-2 py-1 text-[0.6875rem] disabled:text-disabled">Select all</button><button type="button" onClick={() => setAllBatchTracks(false)} disabled={!batchTrackIds.length} className="rounded border border-border px-2 py-1 text-[0.6875rem] disabled:text-disabled">Clear all</button></div></div>
          {batchTrackIds.length ? <div className="mt-2 flex flex-wrap gap-x-4 gap-y-2">{batchTrackIds.map((track) => <BatchTrackCheckbox key={track.id} trackId={track.id} available={track.available} selected={track.selected} onChange={(select) => setBatchTrack(track.id, select)} />)}</div> : <p className="mt-2 text-xs text-muted">Select files with ASS, SSA, or SRT tracks.</p>}
          <p className="mt-2 text-[0.6875rem] leading-4 text-muted">Changes each Track ID across every selected file where that editable ID exists.</p>
        </div>
        <div className="rounded-md border border-warning bg-panel p-3 text-xs leading-5 text-muted">ASS/SSA tracks are restyled directly. SRT tracks are converted to ASS in the same operation. Duplicate selected-font attachments and fonts replaced on the chosen tracks are removed; unrelated fonts, covers, and other attachments are preserved. PGS, VobSub, and other bitmap subtitles have no editable text and will be blocked.</div>
      </div>}
      files={<FileSelectionTable title="File Info" files={mkvFiles} templateFile={null} selection={selection} columns={columns} matchingAction={false} label="Subtitle Format file selection" />}
      details={<>
        <h2 className="shrink-0 text-base font-semibold">Subtitle Preview</h2>
        <div className="mt-3 min-h-0 flex-1 overflow-auto">
          <div className="grid gap-3 md:grid-cols-2">
            <PreviewCard label="Old subtitle format" font="Arial" fontSize={48} color="#FFFFFF" outlineColor="#000000" outlineSize={2} time="00:01:12.000 → 00:01:15.000" />
            <PreviewCard label="New subtitle format" font={fontFamily || "Arial"} fontSize={Number(fontSize) || 48} color={primaryColor} outlineColor={outlineColor} outlineSize={Number(outlineSize) || 0} time={`${shiftedTime.start} → ${shiftedTime.end}`} />
          </div>
          <p className="mt-3 text-xs text-muted">The preview demonstrates the font, size, color, outline, and timing changes. Existing positioning and other ASS style properties are preserved.</p>
        </div>
      </>}
      summary={{ title: "Subtitle Format Preview Summary", emptyText: "Build a preview to see subtitle changes.", available: previewResult !== null, status: previewResult?.status ?? "", summary: previewResult?.summary ?? "", metrics: [{ label: "Files changing", value: new Set(previewResult?.actions.map((action) => action.filePath) ?? []).size, tone: "text-success" }, { label: "Blocked/no change", value: previewResult?.noChangeFiles.length ?? 0, tone: "text-muted" }], sections: [{ title: "Planned operations", emptyText: "No compatible subtitle changes are available.", rows: previewResult?.actions.map((action) => ({ key: `${action.filePath}-${action.index}`, title: action.fileName, detail: action.description })) ?? [] }] }}
    >
      {confirmed.dialog ? <OriginalsConfirm {...confirmed.dialog} /> : null}
    </OperationShell>
  );
}

function formatStatus(editable: number, chosen: number) { return !editable ? "No compatible text subtitles" : chosen ? `${chosen} selected` : "Choose a track"; }
function PreviewCard({ label, font, fontSize, color, outlineColor, outlineSize, time }: { label: string; font: string; fontSize: number; color: string; outlineColor: string; outlineSize: number; time: string }) { const shadow = outlineSize <= 0 ? "none" : `${outlineSize}px 0 ${outlineColor}, -${outlineSize}px 0 ${outlineColor}, 0 ${outlineSize}px ${outlineColor}, 0 -${outlineSize}px ${outlineColor}`; const previewSize = Math.min(64, Math.max(12, fontSize / 2)); return <div className="overflow-hidden rounded-md border border-border bg-window"><div className="border-b border-border px-3 py-2 text-xs font-semibold text-muted">{label}</div><div className="flex min-h-28 items-end justify-center bg-black px-4 py-5 text-center"><span style={{ fontFamily: font, fontSize: `${previewSize}px`, color, textShadow: shadow }}>This is a subtitle preview.</span></div><div className="px-3 py-2 text-xs text-muted">Font: <span className="text-text">{font}</span> · Size: <span className="text-text">{fontSize}</span> · Fill: <span className="text-text">{color}</span> · Outline: <span className="text-text">{outlineColor} / {outlineSize}</span><br/>Timing: <span className="text-text">{time}</span></div></div>; }
function BatchTrackCheckbox({ trackId, available, selected, onChange }: { trackId: number; available: number; selected: number; onChange: (select: boolean) => void }) { const input = useRef<HTMLInputElement>(null); const allSelected = available > 0 && selected === available; useEffect(() => { if (input.current) input.current.indeterminate = selected > 0 && !allSelected; }, [allSelected, selected]); return <label className="flex items-center gap-2 text-xs"><input ref={input} aria-label={`Batch Track ID ${trackId}`} type="checkbox" checked={allSelected} onChange={() => onChange(!allSelected)} /><span>ID {trackId} <span className="text-muted">({selected}/{available})</span></span></label>; }
function ColorField({ label, value, onChange }: { label: string; value: string; onChange: (value: string) => void }) { return <label className="block"><span className="text-xs font-semibold text-muted">{label}</span><span className="mt-1.5 flex h-9 overflow-hidden rounded-md border border-border bg-input"><input aria-label={label} type="color" value={value} onChange={(event) => onChange(event.target.value.toUpperCase())} className="h-full w-11 border-0 bg-transparent p-1"/><input aria-label={`${label} hex`} value={value} onChange={(event) => onChange(event.target.value.toUpperCase())} className="min-w-0 flex-1 bg-transparent px-2 text-xs"/></span></label>; }
function isEditableSubtitle(track: { type: string; codec: string }) { const codec = track.codec.toLowerCase(); return ["subtitle", "subtitles"].includes(track.type.toLowerCase()) && (codec.includes("substation") || codec.includes("subrip") || codec === "ass" || codec === "ssa" || codec === "srt"); }
function isSrtSubtitle(track: { codec: string }) { const codec = track.codec.toLowerCase(); return codec.includes("subrip") || codec === "srt"; }
function fontDto(font: SubtitleFont) { return { id: font.id, family: font.family, fileName: font.fileName, dataBase64: font.dataBase64 ?? "" }; }
function uniqueFonts(fonts: SubtitleFont[]) { return [...new Map(fonts.map((font) => [font.id, font])).values()]; }
function formatOffset(offset: number) { const format = (millis: number) => { const value = Math.max(0, millis); return `${String(Math.floor(value / 3_600_000)).padStart(2, "0")}:${String(Math.floor(value / 60_000) % 60).padStart(2, "0")}:${String(Math.floor(value / 1000) % 60).padStart(2, "0")}.${String(value % 1000).padStart(3, "0")}`; }; return { start: format(72_000 + offset), end: format(75_000 + offset) }; }
