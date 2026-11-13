import { useMemo, useRef, useState } from "react";

import type { MediaFileRow } from "../api";
import { hasTemplateMismatch } from "../lib/templateMatch";

/** Page-local selection that defaults to the template plus the files equal to it. */
export function useFileSelection(files: MediaFileRow[], templateFile: MediaFileRow | null) {
  const differingPaths = useMemo(
    () => new Set(files.filter((file) => hasTemplateMismatch(file, templateFile)).map((file) => file.path)),
    [files, templateFile]
  );
  // Without a template nothing is known to match, so nothing starts selected.
  const defaultPaths = useMemo(
    () => templateFile ? files.filter((file) => !differingPaths.has(file.path)).map((file) => file.path) : [],
    [files, templateFile, differingPaths]
  );
  // A manual selection lasts only while the template and the files equal to it stay the same.
  const scope = [templateFile?.path ?? "", ...defaultPaths].join("\u0000");
  const [manual, setManual] = useState<{ scope: string; paths: string[] } | null>(null);
  const selectedPaths = useMemo(() => {
    if (manual?.scope !== scope) return defaultPaths;
    const chosen = new Set(manual.paths);
    return files.filter((file) => chosen.has(file.path)).map((file) => file.path);
  }, [manual, scope, files, defaultPaths]);
  const anchorPath = useRef("");

  function select(paths: string[]) {
    setManual({ scope, paths });
  }

  return {
    selectedPaths,
    differingPaths,
    selectAll: () => select(files.map((file) => file.path)),
    selectNone: () => select([]),
    selectMatching: () => select(defaultPaths),
    invert: () => {
      const current = new Set(selectedPaths);
      select(files.filter((file) => !current.has(file.path)).map((file) => file.path));
    },
    /** With Shift, every row between the last toggled one and this one takes the new state. */
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
      select([...next]);
    }
  };
}

export type FileSelection = ReturnType<typeof useFileSelection>;
