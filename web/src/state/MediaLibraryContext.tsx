import { useQuery } from "@tanstack/react-query";
import { createContext, ReactNode, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";
import { CurrentScanResponse, getCurrentScanFiles, MediaFileRow, setFileSelection } from "../api";

type MediaLibraryContextValue = {
  files: MediaFileRow[];
  removeFilesFromView: (paths: string[]) => void;
  setFiles: (files: MediaFileRow[]) => void;
  /** Replace the visible operation scope without replacing the backend scan. */
  setWorkingView: (files: MediaFileRow[], selectedPaths: string[], templatePath: string) => void;
  isWorkingView: boolean;
  /**
   * Paths the user has selected to operate on.
   *
   * Rust owns this: it is pushed to the backend on every change and hydrated
   * from `getCurrentScanFiles`, so it is the same on the desktop and in the
   * browser and cannot disagree with the working set an operation runs against.
   * The local copy is a cache that keeps the UI responsive.
   */
  selectedPaths: string[];
  selectionError: string | null;
  setSelectedPaths: (paths: string[]) => void;
  toggleSelectedPath: (path: string) => void;
  /** Adopt the selection the backend reports, without echoing it back. */
  hydrateSelection: (paths: string[]) => void;
  /**
   * Adopt the backend's working set when it is newer than what is held.
   *
   * Rust owns the working set and updates it as operations land, so pages read
   * it rather than guessing. They used to adopt it only while holding nothing,
   * which meant the very first scan won and every later change -- a rename, a
   * property edit -- was invisible until the app was restarted.
   */
  syncFromBackend: (scan: CurrentScanResponse, sourcePaths?: string[]) => void;
  templateFilePath: string;
  setTemplateFilePath: (path: string) => void;
  /** Clear scan-derived UI state without removing settings or source folders. */
  clearLibraryCache: () => void;
  updateFilesAfterRename: (renames: Array<{ oldPath: string; newPath: string; newFileName: string; status?: string }>) => void;
};

type MediaLibraryData = Pick<MediaLibraryContextValue, "files" | "isWorkingView" | "selectedPaths" | "selectionError" | "templateFilePath">;
type MediaLibraryActions = Omit<MediaLibraryContextValue, keyof MediaLibraryData>;

// Two contexts, so a page that only acts is not re-rendered by every file or selection change.
const MediaLibraryDataContext = createContext<MediaLibraryData | null>(null);
const MediaLibraryActionsContext = createContext<MediaLibraryActions | null>(null);
const templateStorageKey = "mkvo.web.templateFilePath";
// Selection outlives the tab: it is a deliberate user action, and losing it
// after a restart means re-picking files across a large library.
const selectionStorageKey = "mkvo.web.selectedPaths";

function readStored<T>(storage: Storage, key: string, fallback: T): T {
  try {
    const value = storage.getItem(key);
    return value ? (JSON.parse(value) as T) : fallback;
  } catch {
    return fallback;
  }
}

function write(storage: Storage, key: string, value: unknown): void {
  try {
    storage.setItem(key, JSON.stringify(value));
  } catch {
    // Storage is a convenience cache; every workflow still works without it.
  }
}

function pathKey(path: string): string {
  return path.replace(/\\/g, "/").toLowerCase();
}

function isPathWithinSource(filePath: string, sourcePath: string): boolean {
  const file = pathKey(filePath).replace(/\/+$/, "");
  const source = pathKey(sourcePath).replace(/\/+$/, "");
  return Boolean(file && source && (file === source || file.startsWith(`${source}/`)));
}

/**
 * Send the selection to the backend, which owns it.
 *
 * A rejected push is logged rather than surfaced: the backend rejects paths it
 * does not know about, and the next `getCurrentScanFiles` hydrates the UI back
 * to the authoritative answer anyway.
 */
export function MediaLibraryProvider({ children }: { children: ReactNode }) {
  // Only paths are kept in the tab. The files come back from the backend
  // working set, which MediaLibrarySynchronizer reads on every load.
  const [files, setFilesState] = useState<MediaFileRow[]>([]);
  const [selectedPaths, setSelectedPathsState] = useState<string[]>(() =>
    readStored<string[]>(localStorage, selectionStorageKey, [])
  );
  // What the adopted set was stamped with, so a repeat of the same answer does
  // not keep replacing local state on every refetch.
  // This is synchronization bookkeeping, not render state. Keeping it in a
  // ref also lets syncFromBackend remain stable: Dashboard depends on that
  // callback, and recreating it whenever files changed caused the cached empty
  // scan response to overwrite a newly completed scan after Clear UI Cache.
  const removedPaths = useRef(new Set(readStored<string[]>(sessionStorage, "mkvo.web.removedPaths", [])));
  const adoptedUpdatedUtc = useRef<string | null>(null);
  const [selectionError, setSelectionError] = useState<string | null>(null);
  const selectionPush = useRef<Promise<void>>(Promise.resolve());
  const workingViewPaths = useRef<Set<string> | null>(null);
  const [isWorkingView, setIsWorkingView] = useState(false);
  const [templateFilePath, setTemplateFilePath] = useState(() => {
    try {
      return sessionStorage.getItem(templateStorageKey) ?? "";
    } catch {
      return "";
    }
  });

  useEffect(() => write(localStorage, selectionStorageKey, selectedPaths), [selectedPaths]);
  // Lets toggleSelectedPath stay stable while reading the latest selection.
  const selectedPathsRef = useRef(selectedPaths);
  useEffect(() => {
    selectedPathsRef.current = selectedPaths;
  }, [selectedPaths]);

  useEffect(() => {
    try {
      sessionStorage.setItem(templateStorageKey, templateFilePath);
    } catch {
      // Session storage is a convenience cache; scans still work without it.
    }
  }, [templateFilePath]);

  // One selection request at a time, in order; `send` answers with the selection the backend stored.
  const queueSelection = useCallback((send: () => Promise<string[]>): void => {
    selectionPush.current = selectionPush.current
      .catch(() => undefined)
      .then(async () => {
        try {
          setSelectedPathsState(await send());
          setSelectionError(null);
        } catch (error) {
          const message = error instanceof Error ? error.message : "The selection could not be saved.";
          setSelectionError(message);
          try {
            const authoritative = await getCurrentScanFiles();
            setSelectedPathsState(authoritative.selectedPaths);
          } catch {
            // Keep the visible error; the next successful refresh reconciles it.
          }
        }
      });
  }, []);
  const pushSelection = useCallback((paths: string[]): void => {
    queueSelection(async () => (await setFileSelection(paths)).selectedPaths);
  }, [queueSelection]);

  const syncFromBackend = useCallback((scan: CurrentScanResponse, sourcePaths?: string[]) => {
    if (scan.updatedUtc && adoptedUpdatedUtc.current && scan.updatedUtc <= adoptedUpdatedUtc.current) {
      return;
    }

    adoptedUpdatedUtc.current = scan.updatedUtc ?? null;
    const viewPaths = workingViewPaths.current;
    const scopedFiles = sourcePaths?.length
      ? scan.files.filter((file) => sourcePaths.some((source) => isPathWithinSource(file.path, source)))
      : scan.files;
    const visibleFiles = scopedFiles.filter((file) => !removedPaths.current.has(pathKey(file.path)));
    const nextFiles = viewPaths
      ? visibleFiles.filter((file) => viewPaths.has(pathKey(file.path)))
      : visibleFiles;
    setFilesState(nextFiles);
    if (viewPaths || sourcePaths?.length || removedPaths.current.size) {
      const available = new Set(nextFiles.map((file) => pathKey(file.path)));
      setSelectedPathsState(scan.selectedPaths.filter((path) => available.has(pathKey(path))));
    } else {
      setSelectedPathsState(scan.selectedPaths);
    }
    setTemplateFilePath((current) => {
      if (nextFiles.length === 0) return "";
      return current && nextFiles.some((file) => pathKey(file.path) === pathKey(current)) ? current : nextFiles[0].path;
    });
  }, []);

  const clearLibraryCache = useCallback(() => {
    removedPaths.current.clear();
    sessionStorage.removeItem("mkvo.web.removedPaths");
    workingViewPaths.current = null;
    setIsWorkingView(false);
    adoptedUpdatedUtc.current = null;
    setFilesState([]);
    setSelectedPathsState([]);
    setTemplateFilePath("");
    setSelectionError(null);
    try {
      sessionStorage.removeItem(templateStorageKey);
      localStorage.removeItem(selectionStorageKey);
    } catch {
      // State is already cleared; storage is only a persistence cache.
    }
  }, []);

  const actions = useMemo<MediaLibraryActions>(() => ({
    removeFilesFromView: (paths) => {
      for (const path of paths) removedPaths.current.add(pathKey(path));
      write(sessionStorage, "mkvo.web.removedPaths", [...removedPaths.current]);
      setFilesState((current) => current.filter((file) => !removedPaths.current.has(pathKey(file.path))));
    },
    setFiles: (nextFiles) => {
      // A direct replacement is a new Dashboard scan or an explicit clear and
      // therefore ends a scoped Library handoff.
      workingViewPaths.current = null;
      setIsWorkingView(false);
      for (const file of nextFiles) removedPaths.current.delete(pathKey(file.path));
      write(sessionStorage, "mkvo.web.removedPaths", [...removedPaths.current]);
      setFilesState(nextFiles);
      setTemplateFilePath((current) => {
        if (nextFiles.length === 0) return "";
        if (current && nextFiles.some((file) => file.path === current)) return current;
        return nextFiles[0].path;
      });
      // A job that remuxes, converts, or renames changes which paths exist.
      // Selections pointing at paths that are gone would silently be sent to
      // the next operation, so the selection is reconciled against the new
      // list rather than left stale. An empty list is a cleared working set,
      // not a reason to forget a selection that will be restored with it.
      if (nextFiles.length > 0) {
        const available = new Set(nextFiles.map((file) => pathKey(file.path)));
        setSelectedPathsState((current) =>
          current.filter((path) => available.has(pathKey(path)))
        );
      }
    },
    setWorkingView: (nextFiles, nextSelectedPaths, nextTemplatePath) => {
      workingViewPaths.current = new Set(nextFiles.map((file) => pathKey(file.path)));
      setIsWorkingView(true);
      setFilesState(nextFiles);
      setSelectedPathsState(nextSelectedPaths);
      setTemplateFilePath(nextTemplatePath || nextFiles[0]?.path || "");
      // A Library scan leaves the backend working set alone, so these files are installed there first.
      queueSelection(async () => {
        const adopted = (await setFileSelection(nextFiles.map((file) => file.path), true)).selectedPaths;
        const readable = new Set(adopted.map(pathKey));
        if (readable.size < nextFiles.length) {
          // The backend left out what it could not read, so those rows cannot be operated on.
          workingViewPaths.current = readable;
          setFilesState((current) => current.filter((file) => readable.has(pathKey(file.path))));
          setTemplateFilePath((current) => readable.has(pathKey(current)) ? current : adopted[0] ?? "");
        }
        const wanted = nextSelectedPaths.filter((path) => readable.has(pathKey(path)));
        return wanted.length < adopted.length ? (await setFileSelection(wanted)).selectedPaths : adopted;
      });
    },
    setSelectedPaths: (paths) => {
      setSelectedPathsState(paths);
      pushSelection(paths);
    },
    toggleSelectedPath: (path) => {
      const selectedPaths = selectedPathsRef.current;
      const next = selectedPaths.some((selected) => pathKey(selected) === pathKey(path))
        ? selectedPaths.filter((selected) => pathKey(selected) !== pathKey(path))
        : [...selectedPaths, path];
      selectedPathsRef.current = next;
      setSelectedPathsState(next);
      pushSelection(next);
    },
    hydrateSelection: (paths) => setSelectedPathsState(paths),
    syncFromBackend,
    setTemplateFilePath,
    clearLibraryCache,
    updateFilesAfterRename: (renames) => {
      if (workingViewPaths.current) {
        workingViewPaths.current = new Set([...workingViewPaths.current].map((path) => {
          const rename = renames.find((item) => pathKey(item.oldPath) === path);
          return rename ? pathKey(rename.newPath) : path;
        }));
      }
      setFilesState((current) => current.map((file) => {
        const rename = renames.find((item) => pathKey(item.oldPath) === pathKey(file.path));
        if (!rename) return file;

        return {
          ...file,
          path: rename.newPath,
          fileName: rename.newFileName,
          status: rename.status ?? "Renamed"
        };
      }));
      // A renamed file is still the file the user picked, so the selection
      // follows it to its new path instead of being dropped.
      setSelectedPathsState((current) => current.map((path) => {
        const rename = renames.find((item) => pathKey(item.oldPath) === pathKey(path));
        return rename ? rename.newPath : path;
      }));
      setTemplateFilePath((current) => {
        const rename = renames.find((item) => pathKey(item.oldPath) === pathKey(current));
        return rename ? rename.newPath : current;
      });
    }
  }), [pushSelection, queueSelection, syncFromBackend, clearLibraryCache]);
  const data = useMemo<MediaLibraryData>(
    () => ({ files, isWorkingView, selectedPaths, selectionError, templateFilePath }),
    [files, isWorkingView, selectedPaths, selectionError, templateFilePath]
  );

  return (
    <MediaLibraryActionsContext.Provider value={actions}>
      <MediaLibraryDataContext.Provider value={data}>
        {children}
      </MediaLibraryDataContext.Provider>
    </MediaLibraryActionsContext.Provider>
  );
}

export function useMediaLibrary(): MediaLibraryContextValue {
  const data = useContext(MediaLibraryDataContext);
  const actions = useMediaLibraryActions();
  if (!data) {
    throw new Error("useMediaLibrary must be used inside MediaLibraryProvider");
  }

  return useMemo(() => ({ ...data, ...actions }), [data, actions]);
}

/** The actions alone: they never change, so using them causes no re-render. */
export function useMediaLibraryActions(): MediaLibraryActions {
  const actions = useContext(MediaLibraryActionsContext);
  if (!actions) {
    throw new Error("useMediaLibrary must be used inside MediaLibraryProvider");
  }

  return actions;
}

export function MediaLibrarySynchronizer() {
  const { syncFromBackend } = useMediaLibraryActions();
  const currentScan = useQuery({ queryKey: ["current-scan-files"], queryFn: getCurrentScanFiles });
  useEffect(() => { if (currentScan.data) syncFromBackend(currentScan.data); }, [currentScan.data, syncFromBackend]);
  return null;
}
