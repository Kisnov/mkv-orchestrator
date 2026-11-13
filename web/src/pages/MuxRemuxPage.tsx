import { useEffect, useMemo, useRef, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import {
  getCurrentScanFiles,
  getWebSettings,
  MediaFileRow,
  MuxPreviewRequest
} from "../api";
import { fileColumns } from "../components/fileColumns";
import { FileSelectionTable } from "../components/FileSelectionTable";
import { LanguageChips } from "../components/LanguageChips";
import { OperationShell } from "../components/OperationShell";
import { pathKey } from "../lib/pathKey";
import { useMediaLibrary } from "../state/MediaLibraryContext";
import { useLibrarySelection } from "../state/useFileSelection";
import { buildMuxRequest, useMuxRequest } from "../state/useMuxRequest";

const columns = [fileColumns.reader, fileColumns.codec, fileColumns.resolution, fileColumns.audio, fileColumns.subtitles, fileColumns.status];

export type MuxWorkflow = "remove" | "subtitles" | "convert";

export function MuxRemuxPage({ workflow = "remove" }: { workflow?: MuxWorkflow }) {
  const { files, setSelectedPaths, templateFilePath, syncFromBackend, isWorkingView } = useMediaLibrary();
  const currentScan = useQuery({ queryKey: ["current-scan-files"], queryFn: getCurrentScanFiles });
  const settings = useQuery({ queryKey: ["web-settings"], queryFn: getWebSettings });
  const [subtitleTab, setSubtitleTab] = useState<"mux" | "extract">("mux");
  const [detailTab, setDetailTab] = useState<"tracks" | "attachments">("tracks");
  const [selectedDetailPath, setSelectedDetailPath] = useState("");
  const [removeAudio, setRemoveAudio] = useState(false);
  const [audioLanguages, setAudioLanguages] = useState("eng,jpn");
  const [removeSubtitles, setRemoveSubtitles] = useState(false);
  const [subtitleLanguages, setSubtitleLanguages] = useState("eng");
  const [removeTrackIds, setRemoveTrackIds] = useState(false);
  const [trackIds, setTrackIds] = useState("");
  const [preserveChapters, setPreserveChapters] = useState(true);
  const [preserveAttachments, setPreserveAttachments] = useState(true);
  const [preserveOriginal, setPreserveOriginal] = useState(() => workflow !== "remove");
  const [remuxOutputSuffix, setRemuxOutputSuffix] = useState(".remuxed");
  const [muxExternal, setMuxExternal] = useState(false);
  const [externalLanguage, setExternalLanguage] = useState("eng");
  const [externalFormats, setExternalFormats] = useState("srt,ass,ssa,sub,idx");
  const [preserveSidecars, setPreserveSidecars] = useState(true);
  const [skipExistingSubtitle, setSkipExistingSubtitle] = useState(false);
  const [extractSubtitles, setExtractSubtitles] = useState(false);
  const previousExtractLanguages = useRef("eng");
  const [extractLanguages, setExtractLanguages] = useState("eng");
  const [extractOverwrite, setExtractOverwrite] = useState(false);
  const [convertMp4, setConvertMp4] = useState(false);
  const [deleteMp4AfterConvert, setDeleteMp4AfterConvert] = useState(false);
  const [previewRemovedTracks, setPreviewRemovedTracks] = useState<Record<string, string[]>>({});
  const [settingsDefaultsApplied, setSettingsDefaultsApplied] = useState(false);
  const languageSuggestions = (type: string) => [...new Set(files.flatMap((file) => file.tracks.filter((track) => type === "subtitle" ? ["subtitle", "subtitles"].includes(track.type.toLowerCase()) : track.type.toLowerCase() === type).map((track) => track.language || "und")))];
  const initializedSelectionScope = useRef("");
  const run = useMuxRequest({
    label: "MKV Operations",
    initialStatus: "Load scanned files from Dashboard, then choose an action.",
    startedText: (total) => `Applying ${total} MKV operation(s)...`,
    noChangesText: "No changes are needed for the selected files.",
    cancelingText: "Canceling MKV operation job...",
    onPreview: (_plan, request) => setPreviewRemovedTracks(buildRemovedTrackDetails(request))
  });

  useEffect(() => {
    if (!currentScan.data) return;
    const scan = currentScan.data;
    const selectionScope = scan.updatedUtc ?? scan.files.map((file) => file.path).join("|");

    syncFromBackend(scan);
    if (scan.files.length === 0) {
      run.setPreviewResult(null);
      setSelectedDetailPath("");
      run.setStatus("Load scanned files from Dashboard, then choose an action.");
    } else if (initializedSelectionScope.current !== selectionScope) {
      // MKV Operations starts every newly loaded scan as a full batch. Record
      // the scan identity first so later user deselection is preserved for the
      // remainder of this batch instead of being immediately undone.
      initializedSelectionScope.current = selectionScope;
      setSelectedPaths((isWorkingView ? files : scan.files).map((file) => file.path));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentScan.data, isWorkingView]);

  useEffect(() => {
    if (!settings.data || settingsDefaultsApplied) return;
    setAudioLanguages(settings.data.mkvMergeDefaultAudioLanguages || "eng,jpn");
    setSubtitleLanguages(settings.data.mkvMergeDefaultSubtitleLanguages || "eng");
    setSettingsDefaultsApplied(true);
  }, [settings.data, settingsDefaultsApplied]);

  useEffect(() => {
    setSelectedDetailPath((current) => current && files.some((file) => file.path === current) ? current : files[0]?.path ?? "");
  }, [files]);

  const mp4Files = useMemo(() => files.filter((file) => file.extension.toLowerCase() === ".mp4"), [files]);
  const pageTitle = workflow === "remove" ? "Remove Tracks" : workflow === "subtitles" ? "Subtitles" : "Convert / Remux";
  const pageDescription = workflow === "remove"
    ? "Remove unwanted audio, subtitle, or specific tracks while preserving the container content you need."
    : workflow === "subtitles"
      ? "Mux external subtitle files or extract embedded subtitles."
      : "Convert MP4 containers to MKV without re-encoding the video or audio.";

  useEffect(() => {
    if (workflow === "convert" && mp4Files.length > 0) setConvertMp4(true);
  }, [workflow, mp4Files.length]);
  const templateFile = useMemo(
    () => files.find((file) => pathKey(file.path) === pathKey(templateFilePath)) ?? files[0] ?? null,
    [files, templateFilePath]
  );
  const selection = useLibrarySelection(files, templateFile);
  const selectedMkvPaths = useMemo(
    () => selection.selectedPaths.filter((path) => files.some((file) => file.path === path && file.extension.toLowerCase() === ".mkv")),
    [files, selection.selectedPaths]
  );
  const selectedMp4Paths = useMemo(
    () => selection.selectedPaths.filter((path) => files.some((file) => file.path === path && file.extension.toLowerCase() === ".mp4")),
    [files, selection.selectedPaths]
  );
  const selectedDetailFile = useMemo(
    () => files.find((file) => file.path === selectedDetailPath) ?? files[0] ?? null,
    [files, selectedDetailPath]
  );
  const canRun = selectedMkvPaths.length > 0 || (convertMp4 && selectedMp4Paths.length > 0);
  const unavailable = (file: MediaFileRow) => file.extension.toLowerCase() !== ".mp4" || convertMp4 ? undefined
    : workflow === "convert" ? "Turn on MP4 conversion to include this file." : "MP4 files can only be converted, on Convert / Remux.";

  function buildRequest(): MuxPreviewRequest {
    return buildMuxRequest(files, convertMp4 ? [...selectedMkvPaths, ...selectedMp4Paths] : selectedMkvPaths, {
      removeUnwantedAudioLanguages: removeAudio,
      keepAudioLanguages: audioLanguages,
      removeUnwantedSubtitleLanguages: removeSubtitles,
      keepSubtitleLanguages: subtitleLanguages,
      removeUnwantedTrackIds: removeTrackIds,
      removeTrackIdsText: trackIds,
      preserveChapters,
      preserveAttachments,
      preserveOriginal: !convertMp4 && !extractSubtitles && preserveOriginal,
      remuxOutputSuffix,
      muxMatchingExternalSubtitles: muxExternal,
      externalSubtitleLanguage: externalLanguage,
      externalSubtitleFormats: externalFormats,
      preserveExternalSubtitleFiles: preserveSidecars,
      skipMuxIfSubtitleAlreadyExists: skipExistingSubtitle,
      extractSubtitles,
      extractSubtitleLanguages: extractLanguages,
      extractOverwriteExistingFiles: extractOverwrite,
      convertMp4ToMkv: convertMp4,
      deleteMp4AfterConvert
    });
  }

  function runPreview() {
    if (!canRun) {
      run.setStatus("MKV Operations requires at least one selected MKV file (or MP4 with conversion enabled).");
      return;
    }
    run.preview(buildRequest());
  }

  const previewResult = run.previewResult;

  return (
    <OperationShell
      title={pageTitle}
      description={pageDescription}
      run={run}
      canRun={canRun}
      onPreview={runPreview}
      onApply={() => { if (canRun) void run.apply(buildRequest()); }}
      selectedText={`${selectedMkvPaths.length + (convertMp4 ? selectedMp4Paths.length : 0)} files selected`}
      hiddenSelected={[...selectedMkvPaths, ...(convertMp4 ? selectedMp4Paths : [])].filter(selection.isHidden).length}
      options={<>
          {workflow !== "subtitles" ? <div className="flex items-center justify-between">
            {workflow === "remove" ? <h2 className="text-base font-semibold">Track Options</h2> : <span />}
          </div> : null}

          {workflow === "remove" ? (
            <div className="mt-3 space-y-2">
              <h2 className="text-sm font-semibold">Track Removal</h2>
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={removeAudio} onChange={(event) => setRemoveAudio(event.target.checked)} /> Remove unwanted audio languages</label>
              <LanguageChips label="Audio languages to keep" value={audioLanguages} onChange={setAudioLanguages} suggestions={languageSuggestions("audio")} />
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={removeSubtitles} onChange={(event) => setRemoveSubtitles(event.target.checked)} /> Remove unwanted subtitle languages</label>
              <LanguageChips label="Subtitle languages to keep" value={subtitleLanguages} onChange={setSubtitleLanguages} suggestions={languageSuggestions("subtitle")} />
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={removeTrackIds} onChange={(event) => setRemoveTrackIds(event.target.checked)} /> Remove unwanted track IDs</label>
              <Field compact label="Track IDs to remove" value={trackIds} onChange={setTrackIds} placeholder="1 or 1, 3" />
              <h2 className="pt-1 text-sm font-semibold">Preservation Options</h2>
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={preserveChapters} onChange={(event) => setPreserveChapters(event.target.checked)} /> Preserve chapters</label>
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={preserveAttachments} onChange={(event) => setPreserveAttachments(event.target.checked)} /> Preserve attachments/fonts</label>
              <OutputOptions compact preserveOriginal={preserveOriginal} setPreserveOriginal={setPreserveOriginal} suffix={remuxOutputSuffix} setSuffix={setRemuxOutputSuffix} />
            </div>
          ) : null}

          {workflow === "subtitles" ? (
            <div className="space-y-3">
              <div role="tablist" aria-label="Subtitle options" className="flex gap-3 border-b border-border">
                {(["mux", "extract"] as const).map((tab) => <button
                  key={tab} type="button" role="tab" id={`subtitle-${tab}-tab`} aria-controls={`subtitle-${tab}-panel`}
                  aria-selected={subtitleTab === tab} tabIndex={subtitleTab === tab ? 0 : -1}
                  onClick={() => setSubtitleTab(tab)}
                  onKeyDown={(event) => {
                    if (["ArrowLeft", "ArrowRight", "Home", "End"].includes(event.key)) {
                      event.preventDefault();
                      const next = event.key === "Home" ? "mux" : event.key === "End" ? "extract" : tab === "mux" ? "extract" : "mux";
                      setSubtitleTab(next); document.getElementById(`subtitle-${next}-tab`)?.focus();
                    }
                  }}
                  className={`border-b-2 pb-2 text-sm font-semibold focus-visible:outline-accent ${subtitleTab === tab ? "border-accent text-text" : "border-transparent text-muted hover:text-text"}`}
                >{tab === "mux" ? "Subtitle Mux" : "Subtitle Extract"}</button>)}
              </div>
              <div role="tabpanel" id="subtitle-mux-panel" aria-labelledby="subtitle-mux-tab" hidden={subtitleTab !== "mux"} className="space-y-3">
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={muxExternal} onChange={(event) => setMuxExternal(event.target.checked)} /> Mux matching external subtitles</label>
              <div className="text-sm text-muted">File Format: <span className="text-accent">file_name.language.tag.ext</span></div>
              <LanguageChips label="Fallback language" value={externalLanguage} onChange={setExternalLanguage} suggestions={languageSuggestions("subtitle")} single />
              <Field label="Subtitle formats" value={externalFormats} onChange={setExternalFormats} placeholder="srt,ass,ssa,sub,idx" />
              <h2 className="pt-1 text-sm font-semibold">Mux Options</h2>
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={preserveSidecars} onChange={(event) => setPreserveSidecars(event.target.checked)} /> Preserve external subtitle files</label>
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={skipExistingSubtitle} onChange={(event) => setSkipExistingSubtitle(event.target.checked)} /> Skip if matching subtitle already exists</label>
              <OutputOptions preserveOriginal={preserveOriginal} setPreserveOriginal={setPreserveOriginal} suffix={remuxOutputSuffix} setSuffix={setRemuxOutputSuffix} />
              <p className="text-xs leading-5 text-muted">Example: Episode 01.eng.Dialogue.ass. See Settings for detailed usage.</p>
              </div>
              <div role="tabpanel" id="subtitle-extract-panel" aria-labelledby="subtitle-extract-tab" hidden={subtitleTab !== "extract"} className="space-y-3">
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={extractSubtitles} onChange={(event) => setExtractSubtitles(event.target.checked)} /> Extract subtitles</label>
              <label className="flex items-center gap-2 text-sm">
                <input type="checkbox" checked={extractLanguages === "all"} onChange={(event) => {
                  if (event.target.checked) { previousExtractLanguages.current = extractLanguages; setExtractLanguages("all"); }
                  else setExtractLanguages(previousExtractLanguages.current);
                }} />
                Extract all subtitles regardless of language
              </label>
              {extractLanguages === "all" ? <p className="text-xs text-muted">Every subtitle track will be extracted, including tracks with an unknown language.</p> : null}
              <LanguageChips label="Subtitle languages" value={extractLanguages} onChange={setExtractLanguages} suggestions={languageSuggestions("subtitle")} allowAll />
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={extractOverwrite} onChange={(event) => setExtractOverwrite(event.target.checked)} /> Overwrite existing extracted files</label>
              </div>
            </div>
          ) : null}

          {workflow === "convert" ? (
            <div className="mt-4 space-y-3 rounded-md border border-accent bg-panel p-3">
              <h2 className="text-sm font-semibold text-accent">MP4 Conversion</h2>
              <p className="text-xs text-muted">{mp4Files.length === 1 ? "1 MP4 file detected" : `${mp4Files.length} MP4 files detected`}</p>
              <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={convertMp4} onChange={(event) => setConvertMp4(event.target.checked)} /> Convert selected MP4 files to MKV</label>
              <label className={["flex items-center gap-2 pl-5 text-sm", convertMp4 ? "" : "text-disabled"].join(" ")}>
                <input type="checkbox" checked={deleteMp4AfterConvert} disabled={!convertMp4} onChange={(event) => setDeleteMp4AfterConvert(event.target.checked)} /> Delete original MP4 after success
              </label>
              <p className="text-xs leading-5 text-muted">Lossless container copy via mkvmerge - no re-encoding. The new .mkv is created next to the source file; files whose .mkv already exists are skipped.</p>
            </div>
          ) : null}
      </>}
      files={<FileSelectionTable title="File Info" files={files} templateFile={templateFile} selection={selection} columns={columns} label="MKV Operations file selection" activePath={selectedDetailFile?.path} onActivate={setSelectedDetailPath} unavailable={unavailable} />}
      details={<>
            <div className="flex shrink-0 gap-6 text-sm">
              <button onClick={() => setDetailTab("tracks")} className={detailTab === "tracks" ? "border-b border-accent pb-1 font-semibold text-text" : "pb-1 font-semibold text-muted"}>File Details: Tracks</button>
              <button onClick={() => setDetailTab("attachments")} className={detailTab === "attachments" ? "border-b border-accent pb-1 font-semibold text-text" : "pb-1 font-semibold text-muted"}>File Details: Attachments</button>
            </div>
            <div className="mt-3 min-h-0 flex-1 overflow-auto">
              {detailTab === "tracks" ? (
                <table className="w-full min-w-[45rem] table-fixed border-collapse text-left text-sm">
                  <thead className="sticky top-0 bg-card text-xs text-text">
                    <tr>
                      <th className="w-12 border-b border-border px-3 py-2">#</th>
                      <th className="w-24 border-b border-border px-3 py-2">Type</th>
                      <th className="w-20 border-b border-border px-3 py-2">Lang</th>
                      <th className="w-44 border-b border-border px-3 py-2">Codec</th>
                      <th className="border-b border-border px-3 py-2">Name</th>
                    </tr>
                  </thead>
                  <tbody>
                    {(selectedDetailFile?.tracks ?? []).map((track) => (
                      <tr key={`${track.id}-${track.trackNumber}`} className="bg-card hover:bg-selected">
                        <td className="border-b border-border px-3 py-2">{track.id}</td>
                        <td className="border-b border-border px-3 py-2">{track.type}</td>
                        <td className="border-b border-border px-3 py-2">{track.language || "und"}</td>
                        <td className="truncate border-b border-border px-3 py-2" title={track.codec}>{track.codec || "Unknown"}</td>
                        <td className="truncate border-b border-border px-3 py-2" title={track.name}>{track.name || "-"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              ) : selectedDetailFile?.attachments?.length ? (
                <table className="w-full min-w-[47.5rem] table-fixed border-collapse text-left text-sm">
                  <thead className="sticky top-0 bg-card text-xs text-text">
                    <tr>
                      <th className="w-14 border-b border-border px-3 py-2">#</th>
                      <th className="w-[16.25rem] border-b border-border px-3 py-2">File</th>
                      <th className="w-52 border-b border-border px-3 py-2">Content Type</th>
                      <th className="w-28 border-b border-border px-3 py-2">Size</th>
                      <th className="border-b border-border px-3 py-2">Description</th>
                    </tr>
                  </thead>
                  <tbody>
                    {(selectedDetailFile.attachments ?? []).map((attachment) => (
                      <tr key={`${attachment.id}-${attachment.fileName}`} className="bg-card hover:bg-selected">
                        <td className="border-b border-border px-3 py-2">{attachment.id}</td>
                        <td className="truncate border-b border-border px-3 py-2" title={attachment.fileName}>{attachment.fileName || "-"}</td>
                        <td className="truncate border-b border-border px-3 py-2" title={attachment.contentType}>{attachment.contentType || "-"}</td>
                        <td className="border-b border-border px-3 py-2">{formatBytes(attachment.sizeBytes)}</td>
                        <td className="truncate border-b border-border px-3 py-2" title={attachment.description}>{attachment.description || "-"}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              ) : (
                <div className="flex h-full min-h-[7.5rem] items-center justify-center text-sm text-subtle">
                  No attachments or fonts were detected for the selected file.
                </div>
              )}
            </div>
      </>}
      summary={{
        title: `${pageTitle} Preview Summary`,
        emptyText: "Build a preview to see planned MKV operations.",
        available: previewResult !== null,
        status: previewResult?.status ?? "",
        summary: previewResult?.summary ?? "",
        metrics: [
          { label: "Files changing", value: new Set(previewResult?.actions.map((action) => action.filePath) ?? []).size, tone: "text-success" },
          { label: "Planned actions", value: previewResult?.actions.length ?? 0, tone: "text-accent" },
          { label: "No change", value: previewResult?.noChangeFiles.length ?? 0, tone: "text-muted" },
          { label: "Tools involved", value: new Set(previewResult?.actions.map((action) => action.toolName) ?? []).size, tone: "text-text" }
        ],
        sections: [
          {
            title: "Planned operations",
            emptyText: "No MKV operations are needed.",
            rows: previewResult?.actions.map((action) => ({
              key: `${action.filePath}-${action.index}`,
              title: action.fileName,
              detail: [
                action.description,
                ...(previewRemovedTracks[pathKey(action.filePath)] ?? [])
              ].join("\n"),
              meta: `${action.operation} · ${action.toolName}`
            })) ?? []
          },
          {
            title: "No change",
            emptyText: "Every selected file requires an operation.",
            rows: previewResult?.noChangeFiles.map((filePath) => ({
              key: filePath,
              title: filePath.split(/[\\/]/).pop() || filePath,
              detail: "No MKV operations are required for this file."
            })) ?? []
          }
        ]
      }}
    />
  );
}

function OutputOptions({ preserveOriginal, setPreserveOriginal, suffix, setSuffix, compact = false }: {
  compact?: boolean;
  preserveOriginal: boolean;
  setPreserveOriginal: (value: boolean) => void;
  suffix: string;
  setSuffix: (value: string) => void;
}) {
  return (
    <div className={compact ? "space-y-2" : "space-y-3"}>
      <h2 className="pt-1 text-sm font-semibold">Output</h2>
      <label className="flex items-center gap-2 text-sm">
        <input type="radio" name="remux-output" checked={preserveOriginal} onChange={() => setPreserveOriginal(true)} />
        Create new file and preserve original
      </label>
      <label className="flex items-center gap-2 text-sm">
        <input type="radio" name="remux-output" checked={!preserveOriginal} onChange={() => setPreserveOriginal(false)} />
        Replace the original
      </label>
      {preserveOriginal ? (
        <Field compact={compact} label="Output suffix" value={suffix} onChange={setSuffix} placeholder=".remuxed" />
      ) : (
        <p className="text-xs leading-5 text-muted">Replacement uses a validated temporary file with automatic rollback.</p>
      )}
    </div>
  );
}

type RemovalPreviewInput = Pick<MuxPreviewRequest,
  "files"
  | "selectedPaths"
  | "removeUnwantedAudioLanguages"
  | "keepAudioLanguages"
  | "removeUnwantedSubtitleLanguages"
  | "keepSubtitleLanguages"
  | "removeUnwantedTrackIds"
  | "removeTrackIdsText"
>;

export function buildRemovedTrackDetails(request: RemovalPreviewInput): Record<string, string[]> {
  const selected = new Set(request.selectedPaths.map(pathKey));
  const explicitIds = request.removeUnwantedTrackIds
    ? new Set((request.removeTrackIdsText.match(/\d+/g) ?? []).map(Number))
    : new Set<number>();
  const audioLanguages = parseLanguageSet(request.keepAudioLanguages);
  const subtitleLanguages = parseLanguageSet(request.keepSubtitleLanguages);
  const details: Record<string, string[]> = {};

  for (const file of request.files) {
    const key = pathKey(file.path);
    if (!selected.has(key)) continue;
    const removed = file.tracks.filter((track) => {
      if (explicitIds.has(track.id)) return true;
      const type = track.type.toLowerCase();
      const language = (track.language || "und").toLowerCase();
      if (type === "audio" && request.removeUnwantedAudioLanguages) return !audioLanguages.has(language);
      if ((type === "subtitle" || type === "subtitles") && request.removeUnwantedSubtitleLanguages) return !subtitleLanguages.has(language);
      return false;
    });
    if (removed.length === 0) continue;
    details[key] = [
      `Removed tracks (${removed.length}):`,
      ...removed.map((track) => {
        const parts = [
          `ID ${track.id}`,
          track.type || "Track",
          track.language || "und",
          track.codec || "Unknown codec",
          track.name || "No name"
        ];
        if (track.default) parts.push("default");
        if (track.forced) parts.push("forced");
        return `• ${parts.join(" · ")}`;
      })
    ];
  }
  return details;
}

function parseLanguageSet(value: string) {
  return new Set(value.split(/[\s,;]+/).map((part) => part.trim().toLowerCase()).filter(Boolean));
}

function Field({ label, value, onChange, placeholder, compact = false }: { label: string; value: string; onChange: (value: string) => void; placeholder: string; compact?: boolean }) {
  return (
    <label className="block">
      <span className="text-xs font-semibold text-muted">{label}</span>
      <input
        value={value}
        onChange={(event) => onChange(event.target.value)}
        placeholder={placeholder}
        className={`${compact ? "mt-1 h-8" : "mt-1.5 h-9"} w-full rounded-md border border-border bg-input px-3 text-sm text-text outline-none placeholder:text-subtle focus:border-accent`}
      />
    </label>
  );
}

function formatBytes(value: number | null | undefined) {
  if (!value || value <= 0) return "-";

  const units = ["B", "KB", "MB", "GB"];
  let size = value;
  let unitIndex = 0;
  while (size >= 1024 && unitIndex < units.length - 1) {
    size /= 1024;
    unitIndex += 1;
  }

  return `${size >= 10 || unitIndex === 0 ? size.toFixed(0) : size.toFixed(1)} ${units[unitIndex]}`;
}
