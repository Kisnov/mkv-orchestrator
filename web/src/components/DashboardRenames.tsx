import { Link } from "react-router-dom";

import type { RenameBatchListResponse } from "../api";
import { timeAgo } from "../lib/jobActivity";
import { DashboardCard } from "./DashboardCard";
import { rowAction } from "./dashboardStyles";

const shownBatches = 3;

/** The latest rename batches; undoing one happens in Rename Files, which shows the restore plan first. */
export function DashboardRenames({ batches, now }: { batches: RenameBatchListResponse["batches"]; now: number }) {
  return (
    <DashboardCard title="Recent renames">
      <ul>
        {batches.slice(0, shownBatches).map((batch) => {
          // A batch is named by its date; the first file it renamed says what it was.
          const name = batch.entries[0]?.renamedFileName ?? batch.displayName;
          return (
            <li key={batch.id} className="flex items-start gap-3 border-b border-border px-4 py-2.5 last:border-b-0">
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium text-text" title={name}>{name}</div>
                <div className="text-xs tabular-nums text-text">{batch.totalFiles} {batch.totalFiles === 1 ? "file" : "files"}, {timeAgo(batch.createdAt, now)}</div>
              </div>
              {batch.isUndone
                ? <span className="text-xs text-muted">Undone</span>
                : <Link to="/rename" state={{ undoBatchId: batch.id }} className={rowAction} aria-label={`Undo the rename of ${name}`}>Undo</Link>}
            </li>
          );
        })}
      </ul>
    </DashboardCard>
  );
}
