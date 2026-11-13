import { useState } from "react";

import type { MuxActionRow, MuxPreviewRequest, MuxPreviewResponse } from "../api";
import { pathKey } from "../lib/pathKey";
import type { useMuxRequest } from "./useMuxRequest";

type Files = Array<{ path: string; name: string }>;
export type OriginalsAtRisk = { replaced: Files; deletedMp4: Files; deletedSubtitles: Files; overwrittenExtracts: Files };

/** The original files a plan overwrites or deletes; null when it keeps them all. */
export function originalsAtRisk(request: MuxPreviewRequest, plan: MuxPreviewResponse): OriginalsAtRisk | null {
  const files = (matches: (action: MuxActionRow) => boolean) => [...new Map(plan.actions.filter(matches).map((action) => [action.filePath, action.fileName])).entries()].map(([path, name]) => ({ path, name }));
  const risk = {
    replaced: request.preserveOriginal ? [] : files((action) => action.operation === "Remux" || action.operation === "Mux subtitles"),
    deletedMp4: request.deleteMp4AfterConvert ? files((action) => action.operation === "Convert to MKV") : [],
    deletedSubtitles: request.preserveExternalSubtitleFiles ? [] : plan.actions.filter((action) => action.operation === "Mux subtitles").flatMap((action) => muxedSubtitles(request, action)),
    // The plan cannot say which outputs exist yet, only which files are extracted.
    overwrittenExtracts: request.extractOverwriteExistingFiles ? files((action) => action.operation === "Extract subtitles") : []
  };
  return Object.values(risk).some((group) => group.length) ? risk : null;
}

// The plan lists muxed subtitles by file name ("• name (details)"); hand-picked ones are known by path.
function muxedSubtitles(request: MuxPreviewRequest, action: MuxActionRow): Files {
  const picked = new Map(request.manualSubtitleSelections.filter((selection) => pathKey(selection.targetPath) === pathKey(action.filePath)).map((selection) => [selection.subtitlePath.split(/[\\/]/).pop() ?? "", selection.subtitlePath]));
  const names = action.description.split("\n").filter((line) => line.startsWith("• ")).map((line) => line.slice(2).replace(/ \([^()]*\)$/, ""));
  if (!names.length && !picked.size) return [{ path: action.filePath, name: `Muxed into ${action.fileName}` }];
  return [...new Set([...names, ...picked.keys()])].map((name) => {
    const path = picked.get(name);
    return path ? { path, name: path } : { path: `${action.filePath}|${name}`, name };
  });
}

/** Applies a plan at once, or holds it for `OriginalsConfirm` when it would replace or delete originals. */
export function useConfirmedApply(run: ReturnType<typeof useMuxRequest>) {
  const [pending, setPending] = useState<{ request: MuxPreviewRequest; plan: MuxPreviewResponse; risk: OriginalsAtRisk } | null>(null);
  return {
    apply: (request: MuxPreviewRequest) => run.apply(request, (plan) => {
      const risk = originalsAtRisk(request, plan);
      if (risk) setPending({ request, plan, risk });
      return !risk;
    }),
    dialog: pending && {
      risk: pending.risk,
      onConfirm: () => { setPending(null); run.applyPlan(pending.request, pending.plan).catch(() => { /* Mutation handlers display the error. */ }); },
      onClose: () => setPending(null)
    }
  };
}
