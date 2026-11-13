import type { OriginalsAtRisk } from "../state/useConfirmedApply";
import { ConfirmModal } from "./ConfirmModal";

/** Lists the original files an apply would replace or delete, and asks before it runs. */
export function OriginalsConfirm({ risk, onConfirm, onClose }: { risk: OriginalsAtRisk; onConfirm: () => void; onClose: () => void }) {
  const count = new Set(Object.values(risk).flat().map((file) => file.path)).size;
  const noun = count === 1 ? "file" : "files";
  const groups = [
    { title: "Replaced", files: risk.replaced, text: "Each file is rebuilt in a temporary copy, which replaces the original once it checks out. The previous version is not kept." },
    { title: "MP4 deleted after conversion", files: risk.deletedMp4, text: "Each MP4 is deleted once its MKV is created." },
    { title: "External subtitles deleted after muxing into", files: risk.deletedSubtitlesOf, text: "The matching subtitle files next to these MKVs are deleted once they are muxed in." },
    { title: "Extracted with overwrite", files: risk.overwrittenExtracts, text: "Subtitle files already extracted under these names are overwritten." }
  ].filter((group) => group.files.length);
  const only = groups.length === 1 ? groups[0].files : null;

  return (
    <ConfirmModal
      title={only === risk.replaced ? `Replace ${count} original ${noun}?` : only === risk.deletedMp4 ? `Delete ${count} original MP4 ${noun}?` : only === risk.overwrittenExtracts ? `Overwrite subtitles extracted earlier from ${count} ${noun}?` : `Replace or delete originals for ${count} ${noun}?`}
      message={`${groups.map((group) => group.text).join(" ")} This can't be undone.`}
      confirmLabel={only === risk.replaced ? `Replace ${count} ${noun}` : only === risk.deletedMp4 ? `Delete ${count} MP4 ${noun}` : only === risk.overwrittenExtracts ? `Extract and overwrite` : `Apply to ${count} ${noun}`}
      onConfirm={onConfirm}
      onClose={onClose}
    >
      <div className="mt-4 space-y-3">
        {groups.map((group) => (
          <section key={group.title} className="overflow-hidden rounded-md border border-warning bg-input">
            <h3 className="border-b border-border px-4 py-2 text-xs font-semibold text-warning">{group.title} ({group.files.length})</h3>
            <ul className="max-h-48 overflow-y-auto">
              {group.files.map((file) => <li key={file.path} className="truncate border-b border-border px-4 py-2 text-sm text-text last:border-0" title={file.path}>{file.name}</li>)}
            </ul>
          </section>
        ))}
      </div>
    </ConfirmModal>
  );
}
