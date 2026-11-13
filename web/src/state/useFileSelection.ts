import { useMemo, useRef, useState } from "react";

import type { MediaFileRow } from "../api";
import { pathKey } from "../lib/pathKey";
import { hasTemplateMismatch } from "../lib/templateMatch";
import { useMediaLibrary } from "./MediaLibraryContext";

export type FileSelection = {
  /** Selected paths among the files, in their order. */
  selectedPaths: string[];
  differingPaths: Set<string>;
  /** The template and the files equal to it; empty without a template. */
  matchingPaths: string[];
  /** Replaces the selection among `scope`, leaving the other files as they are. */
  setWithin: (scope: string[], chosen: string[]) => void;
  /** With Shift, every row between the last toggled one and this one takes the new state. */
  toggle: (path: string, shiftKey: boolean, displayedPaths: string[]) => void;
  /** The table's name filter: kept here so a page can say what the filter hides. */
  filter: string;
  setFilter: (filter: string) => void;
  isHidden: (path: string) => boolean;
};

export function matchesFilter(file: MediaFileRow, filter: string) {
  const needle = filter.trim().toLowerCase();
  return !needle || file.fileName.toLowerCase().includes(needle);
}

function useFilter(files: MediaFileRow[]) {
  const [filter, setFilter] = useState("");
  const hidden = useMemo(() => new Set(files.filter((file) => !matchesFilter(file, filter)).map((file) => file.path)), [files, filter]);
  return { filter, setFilter, isHidden: (path: string) => hidden.has(path) };
}

function useTemplateMatch(files: MediaFileRow[], templateFile: MediaFileRow | null) {
  const differingPaths = useMemo(
    () => new Set(files.filter((file) => hasTemplateMismatch(file, templateFile)).map((file) => file.path)),
    [files, templateFile]
  );
  // Without a template nothing is known to match.
  const matchingPaths = useMemo(
    () => templateFile ? files.filter((file) => !differingPaths.has(file.path)).map((file) => file.path) : [],
    [files, templateFile, differingPaths]
  );
  return { differingPaths, matchingPaths };
}

function useSelectionActions(selectedPaths: string[], commit: (paths: string[]) => void) {
  const anchorPath = useRef("");
  return {
    setWithin: (scope: string[], chosen: string[]) => {
      const replaced = new Set(scope);
      commit([...selectedPaths.filter((path) => !replaced.has(path)), ...chosen]);
    },
    toggle: (path: string, shiftKey: boolean, displayedPaths: string[]) => {
      const checked = !selectedPaths.includes(path);
      const index = displayedPaths.indexOf(path);
      const anchorIndex = shiftKey ? displayedPaths.indexOf(anchorPath.current) : -1;
      const range = anchorIndex < 0 ? [path] : displayedPaths.slice(Math.min(anchorIndex, index), Math.max(anchorIndex, index) + 1);
      if (anchorIndex < 0) anchorPath.current = path;
      const next = new Set(selectedPaths);
      for (const item of range) {
        if (checked) next.add(item);
        else next.delete(item);
      }
      commit([...next]);
    }
  };
}

/** Page-local selection that defaults to the template plus the files equal to it. */
export function useFileSelection(files: MediaFileRow[], templateFile: MediaFileRow | null): FileSelection {
  const { differingPaths, matchingPaths } = useTemplateMatch(files, templateFile);
  // A manual selection lasts only while the template and the files equal to it stay the same.
  const scope = [templateFile?.path ?? "", ...matchingPaths].join("\u0000");
  const [manual, setManual] = useState<{ scope: string; paths: string[] } | null>(null);
  // Dropped on the first change, so returning to an earlier scope does not bring it back.
  if (manual && manual.scope !== scope) setManual(null);
  const selectedPaths = useMemo(() => {
    if (manual?.scope !== scope) return matchingPaths;
    const chosen = new Set(manual.paths);
    return files.filter((file) => chosen.has(file.path)).map((file) => file.path);
  }, [manual, scope, files, matchingPaths]);
  const actions = useSelectionActions(selectedPaths, (paths) => setManual({ scope, paths }));
  return { selectedPaths, differingPaths, matchingPaths, ...actions, ...useFilter(files) };
}

/**
 * The library's shared selection, seen through these files: selecting here
 * leaves the selection of files outside them alone.
 */
export function useLibrarySelection(files: MediaFileRow[], templateFile: MediaFileRow | null): FileSelection {
  const { selectedPaths: shared, setSelectedPaths } = useMediaLibrary();
  const { differingPaths, matchingPaths } = useTemplateMatch(files, templateFile);
  const selectedPaths = useMemo(() => {
    const keys = new Set(shared.map(pathKey));
    return files.filter((file) => keys.has(pathKey(file.path))).map((file) => file.path);
  }, [files, shared]);
  const actions = useSelectionActions(selectedPaths, (paths) => {
    const own = new Set(files.map((file) => pathKey(file.path)));
    setSelectedPaths([...shared.filter((path) => !own.has(pathKey(path))), ...paths]);
  });
  return { selectedPaths, differingPaths, matchingPaths, ...actions, ...useFilter(files) };
}
