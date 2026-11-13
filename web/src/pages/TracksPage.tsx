import { lazy, Suspense, useEffect, useRef } from "react";
import { Loader2 } from "lucide-react";
import { Navigate, NavLink, useParams } from "react-router-dom";

import { useMediaLibrary } from "../state/MediaLibraryContext";

const MuxRemuxPage = lazy(() => import("./MuxRemuxPage").then((page) => ({ default: page.MuxRemuxPage })));
const ReorderTracksPage = lazy(() => import("./ReorderTracksPage").then((page) => ({ default: page.ReorderTracksPage })));
const SubtitleFormatPage = lazy(() => import("./SubtitleFormatPage").then((page) => ({ default: page.SubtitleFormatPage })));
const TrackPropertiesPage = lazy(() => import("./TrackPropertiesPage").then((page) => ({ default: page.TrackPropertiesPage })));

const operations = [
  { op: "properties", label: "Edit Tracks", page: () => <TrackPropertiesPage /> },
  { op: "reorder", label: "Reorder", page: () => <ReorderTracksPage /> },
  { op: "subtitle-format", label: "Subtitle Format", page: () => <SubtitleFormatPage /> },
  { op: "remove", label: "Remove Tracks", page: () => <MuxRemuxPage workflow="remove" /> },
  { op: "convert", label: "Convert / Remux", page: () => <MuxRemuxPage workflow="convert" />, requiresMp4: true }
];

/** One place for the operations that change the tracks of the scanned files. */
export function TracksPage() {
  const { op } = useParams();
  const { files } = useMediaLibrary();
  const tabsRef = useRef<HTMLElement>(null);
  // A narrow strip may have scrolled the active tab out of sight.
  useEffect(() => {
    tabsRef.current?.querySelector('[aria-current="page"]')?.scrollIntoView?.({ block: "nearest", inline: "nearest" });
  }, [op]);
  const current = operations.find((operation) => operation.op === op);
  if (!current) return <Navigate to="/tracks/properties" replace />;
  const hasMp4Files = files.some((file) => file.extension.toLowerCase() === ".mp4");

  return (
    <div className="flex h-full min-h-0 flex-col">
      <nav ref={tabsRef} aria-label="Track operations" className="mb-5 flex shrink-0 gap-x-5 gap-y-1 overflow-x-auto shadow-[inset_0_-1px_0_var(--color-border)] max-md:flex-wrap">
        {operations.filter((operation) => !operation.requiresMp4 || hasMp4Files || operation === current).map((operation) => (
          <NavLink
            key={operation.op}
            to={`/tracks/${operation.op}`}
            className={({ isActive }) => `shrink-0 whitespace-nowrap border-b-2 pb-2 text-sm font-semibold transition ${isActive ? "border-accent text-text" : "border-transparent text-muted hover:text-text"}`}
          >
            {operation.label}
          </NavLink>
        ))}
      </nav>
      <div className="min-h-0 flex-1">
        <Suspense fallback={<div className="grid h-full place-items-center"><Loader2 className="size-6 animate-spin text-accent" /></div>}>
          {/* Each operation is its own component, so Remove and Convert, one page in two modes, start fresh. */}
          <current.page />
        </Suspense>
      </div>
    </div>
  );
}
