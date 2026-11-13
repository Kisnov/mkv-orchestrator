import { useState } from "react";

import type { MuxActionRow, MuxPreviewRequest, MuxPreviewResponse } from "../api";
import type { useMuxRequest } from "./useMuxRequest";

type Files = Array<{ path: string; name: string }>;
export type OriginalsAtRisk = { replaced: Files; deletedMp4: Files; deletedSubtitlesOf: Files };

/** The original files a plan overwrites or deletes; null when it keeps them all. */
export function originalsAtRisk(request: MuxPreviewRequest, plan: MuxPreviewResponse): OriginalsAtRisk | null {
  const files = (matches: (action: MuxActionRow) => boolean) => [...new Map(plan.actions.filter(matches).map((action) => [action.filePath, action.fileName])).entries()].map(([path, name]) => ({ path, name }));
  const risk = {
    replaced: request.preserveOriginal ? [] : files((action) => action.operation === "Remux" || action.operation === "Mux subtitles"),
    deletedMp4: request.deleteMp4AfterConvert ? files((action) => action.operation === "Convert to MKV") : [],
    deletedSubtitlesOf: request.preserveExternalSubtitleFiles ? [] : files((action) => action.operation === "Mux subtitles")
  };
  return risk.replaced.length || risk.deletedMp4.length || risk.deletedSubtitlesOf.length ? risk : null;
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
