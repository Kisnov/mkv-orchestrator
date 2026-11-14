import { useMemo } from "react";
import { Link } from "react-router-dom";

import type { MediaFileRow } from "../api";
import { scanStats, type Share } from "../lib/scanStats";
import { hasTemplateMismatch } from "../lib/templateMatch";
import { DashboardCard } from "./DashboardCard";
import { linkButton } from "./dashboardStyles";

const shownShares = 3;

function ShareList({ title, shares, total }: { title: string; shares: Share[]; total: number }) {
  if (shares.length === 0) return null;
  return (
    <div className="min-w-0">
      <h3 className="text-xs font-semibold text-muted">{title}</h3>
      <ul className="mt-1.5 space-y-1.5">
        {shares.slice(0, shownShares).map((share) => (
          <li key={share.label} className="grid grid-cols-[minmax(0,5.5rem)_minmax(1rem,1fr)_auto] items-center gap-x-2 text-xs">
            <span className="truncate text-text" title={share.label}>{share.label}</span>
            <span aria-hidden="true" className="h-1 overflow-hidden rounded-full bg-input">
              <span className="block h-full rounded-full bg-accent/70" style={{ width: `${Math.max(4, (share.count / total) * 100)}%` }} />
            </span>
            <span className="min-w-[1.25rem] text-right tabular-nums text-muted">{share.count}</span>
          </li>
        ))}
        {shares.length > shownShares ? <li className="text-xs text-muted">and {shares.length - shownShares} more</li> : null}
      </ul>
    </div>
  );
}

/** What the scanned files are made of, counted from the rows the app already holds. */
export function DashboardScannedFiles({ files, templateFilePath, sources, isScanning }: {
  files: MediaFileRow[];
  /** A scan runs: its files replace these when it ends. */
  isScanning: boolean;
  templateFilePath: string;
  /** The folders or files the scan was given. */
  sources: string[];
}) {
  const stats = useMemo(() => scanStats(files), [files]);
  const template = useMemo(() => files.find((file) => file.path === templateFilePath) ?? files[0] ?? null, [files, templateFilePath]);
  const differing = useMemo(() => files.filter((file) => file !== template && hasTemplateMismatch(file, template)).length, [files, template]);
  const readable = stats.total - stats.unreadable;
  const sourceNames = sources.map((path) => path.replace(/[\\/]+$/, "").split(/[\\/]+/).pop() || path);

  if (files.length === 0) {
    return (
      <DashboardCard title="Scanned files">
        <div className="px-4 py-6">
          <p className="text-sm font-semibold text-text">{isScanning ? "A scan is running" : "No files scanned yet"}</p>
          <p className="mt-1 text-sm text-muted">{isScanning ? "Its files appear here when it finishes." : "Scan a folder to see what is in it, then rename or edit its files."}</p>
          <Link to="/dashboard/scan" className={`mt-4 ${linkButton}`}>{isScanning ? "Open Scan" : "Scan a folder"}</Link>
        </div>
      </DashboardCard>
    );
  }

  return (
    <DashboardCard title="Scanned files" action={<Link to="/dashboard/scan" className={linkButton}>Open Scan</Link>}>
      <div className="px-4 py-3">
        <p className="flex flex-wrap items-baseline gap-x-4 gap-y-1 text-sm text-muted">
          <span><span className="text-xl font-semibold tabular-nums text-text">{stats.total}</span> {stats.total === 1 ? "file" : "files"}</span>
          {stats.mkv ? <span className="tabular-nums">{stats.mkv} MKV</span> : null}
          {stats.mp4 ? <span className="tabular-nums">{stats.mp4} MP4</span> : null}
          {stats.hdr ? <span className="tabular-nums">{stats.hdr} HDR</span> : null}
          {stats.unreadable ? <span className="tabular-nums text-warning">{stats.unreadable} unreadable</span> : null}
        </p>
        {sourceNames.length ? <p className="mt-1 truncate text-xs text-muted" title={sources.join("\n")}>From {sourceNames.join(", ")}</p> : null}
        {template ? (
          <p className="mt-2 text-xs text-muted">
            <span>Template </span>
            <span className="break-words text-template [overflow-wrap:anywhere]">{template.fileName}</span>
            {differing
              ? <span className="text-warning">, {differing} {differing === 1 ? "file differs" : "files differ"} from it</span>
              : files.length > 1 ? <span>, every file matches it</span> : null}
          </p>
        ) : null}
        <div className="mt-4 grid grid-cols-2 gap-x-5 gap-y-4">
          <ShareList title="Video" shares={stats.video} total={readable} />
          <ShareList title="Resolution" shares={stats.resolution} total={readable} />
          <ShareList title="Audio languages" shares={stats.audio} total={readable} />
          <ShareList title="Subtitle languages" shares={stats.subtitles} total={readable} />
        </div>
      </div>
      <nav aria-label="Work on the scanned files" className="flex flex-wrap gap-2 border-t border-border px-4 py-2.5">
        <Link to="/rename" className={linkButton}>Rename Files</Link>
        <Link to="/tracks" className={linkButton}>Tracks</Link>
        <Link to="/subtitles" className={linkButton}>Subtitles</Link>
      </nav>
    </DashboardCard>
  );
}
