import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { Route, Routes } from "react-router-dom";

import { buildLibraryTitles, LibraryPage, resetLibraryViewCacheForTests } from "./LibraryPage";
import { DashboardPage } from "./DashboardPage";
import { MediaLibraryProvider } from "../state/MediaLibraryContext";
import { renderWithBackend } from "../test/render";
import { ApiError } from "../api";
import type { LibraryAuditRow, MediaFileRow, ScanJobResponse, WebSettings } from "../api";
import type { BackendClient } from "../backend/client";

function auditRow(season: number, hasIssues: boolean): LibraryAuditRow {
  const root = `/media/tv/Example Show/Season ${season}`;
  return {
    folderPath: root,
    folderName: `Example Show / Season ${season}`,
    fileCount: 2,
    standardVideo: "1920x1080 HEVC/H.265 10bit",
    standardAudio: "eng:E-AC-3",
    standardSubtitles: "eng:SubRip/SRT",
    templateFilePath: `${root}/Episode 01.mkv`,
    templateFileName: "Episode 01.mkv",
    hasIssues,
    issueSummary: hasIssues ? "1 issue(s)" : "Standard",
    issues: hasIssues ? ["Episode 02.mkv: subtitles mismatch"] : [],
    issueFilePaths: hasIssues ? [`${root}/Episode 02.mkv`] : [],
    allFilePaths: [`${root}/Episode 01.mkv`, `${root}/Episode 02.mkv`]
  };
}

function media(path: string): MediaFileRow {
  return {
    path,
    fileName: path.split("/").pop()!,
    extension: ".mkv",
    status: "Scanned",
    reader: "mkvmerge",
    codec: "HEVC/H.265",
    resolution: "1920x1080",
    bitDepth: "10",
    hdr: "None",
    videoSummary: "HEVC/H.265",
    audioSummary: "eng x1",
    subtitleSummary: "eng x1",
    attachmentSummary: "None",
    tracks: [],
    attachments: []
  };
}

function settings(): WebSettings {
  return {
    watchFolders: [],
    ignoredScanFolderNames: [],
    mediaServers: [{
      id: "server-1",
      name: "Jellyfin",
      type: "Jellyfin",
      serverUrl: "http://jellyfin",
      hasApiKey: true,
      isDefault: true,
      lastSyncedUtc: null,
      libraries: [{ id: "tv-1", name: "TV Shows", type: "tvshows", serverPath: "/srv/tv", containerPath: "/media/tv", isEnabled: true }]
    }]
  } as WebSettings;
}

beforeEach(() => {
  resetLibraryViewCacheForTests();
  window.localStorage.clear();
  window.sessionStorage.clear();
});

describe("Library title aggregation", () => {
  it("combines every season into one series poster and matches media-server metadata", () => {
    const titles = buildLibraryTitles(
      [auditRow(1, true), auditRow(2, false)],
      [{ id: "series-1", title: "Example Show", year: 2024, mediaType: "series", hasPoster: true }]
    );

    expect(titles).toHaveLength(1);
    expect(titles[0]).toMatchObject({ title: "Example Show", fileCount: 4, mismatchFileCount: 1, hasIssues: true });
    expect(titles[0].seasons).toHaveLength(2);
    expect(titles[0].catalogItem?.id).toBe("series-1");
  });

  it("lists each file of a title once across all its seasons", () => {
    const seasons = Array.from({ length: 300 }, (_, index) => ({ ...auditRow(index + 1, false), allFilePaths: ["/media/tv/Example Show/shared.mkv", `/media/tv/Example Show/${index}.mkv`] }));

    const [title] = buildLibraryTitles(seasons, []);

    expect(title.allFilePaths).toHaveLength(301);
    expect(title.allFilePaths[0]).toBe("/media/tv/Example Show/shared.mkv");
  });

  it("combines audit groups whose folder labels include the series and season", () => {
    const season1 = {
      ...auditRow(1, false),
      folderPath: "/media/tv/Roseanne (1988)/Roseanne - Season 01",
      folderName: "Roseanne - Season 01 / movie/single folder"
    };
    const season2 = {
      ...auditRow(2, false),
      folderPath: "/media/tv/Roseanne (1988)/Roseanne - Season 02",
      folderName: "Roseanne - Season 02 / movie/single folder"
    };

    const titles = buildLibraryTitles([season1, season2], []);

    expect(titles).toHaveLength(1);
    expect(titles[0].title).toBe("Roseanne (1988)");
    expect(titles[0].seasons).toHaveLength(2);
  });
});

describe("Library poster workflow", () => {
  it("replaces the previous Dashboard scan during a mismatch handoff", async () => {
    const user = userEvent.setup();
    const row = auditRow(1, true);
    const handoffRoot = "/media/handoff/Example Show/Season 1";
    const rows = [{
      ...row,
      folderPath: handoffRoot,
      templateFilePath: `${handoffRoot}/Episode 01.mkv`,
      issueFilePaths: [`${handoffRoot}/Episode 02.mkv`],
      allFilePaths: [`${handoffRoot}/Episode 01.mkv`, `${handoffRoot}/Episode 02.mkv`]
    }];
    const files = rows.flatMap((row) => row.allFilePaths.map(media));
    const previous = media("/media/old/Previous Scan.mkv");
    let workingSet = [previous];
    const handoffSettings = settings();
    handoffSettings.mediaServers[0].libraries[0].containerPath = "/media/handoff";
    const completedJob = {
      id: "library-scan",
      status: "Completed",
      completed: files.length,
      total: files.length,
      files,
      summary: { total: files.length, mkv: files.length, mp4: 0, failed: 0, cached: 0 },
      skipped: [],
      error: ""
    } as ScanJobResponse;

    renderWithBackend(
      <MediaLibraryProvider>
        <Routes>
          <Route path="/" element={<LibraryPage />} />
          <Route path="/dashboard" element={<DashboardPage />} />
        </Routes>
      </MediaLibraryProvider>,
      {
        getStatus: () => Promise.resolve({ name: "MKVO", version: "0.1.0", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 }),
        getWebSettings: () => Promise.resolve(handoffSettings),
        getLibraryCatalog: () => Promise.resolve({ items: [] }),
        getLibraryLocalArtwork: () => Promise.reject(new Error("No local poster")),
        startScan: () => Promise.resolve({ ...completedJob, status: "Queued", files: [] }),
        getScanJob: () => Promise.resolve(completedJob),
        buildLibraryAudit: () => Promise.resolve({ summary: { groups: 1, files: files.length, issueGroups: 1, standardGroups: 0 }, items: rows }),
        setFileSelection: (selectedPaths, replaceWorkingSet) => {
          if (replaceWorkingSet) workingSet = files.filter((file) => selectedPaths.includes(file.path));
          return Promise.resolve({ selectedPaths });
        },
        getCurrentScanFiles: () => Promise.resolve({
          updatedUtc: workingSet[0] === previous ? "2026-08-20T20:00:00Z" : "2026-08-28T20:00:01Z",
          files: workingSet,
          selectedPaths: workingSet.map((file) => file.path),
          summary: { total: workingSet.length, mkv: workingSet.length, mp4: 0, failed: 0, cached: 0 }
        })
      }
    );

    await user.click(await screen.findByRole("button", { name: /open example show library details/i }));
    await user.click(screen.getByRole("button", { name: /send mismatches \+ template to dashboard/i }));

    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
    expect(await screen.findByRole("row", { name: /Episode 01\.mkv/i })).toBeInTheDocument();
    expect(screen.queryByText("Previous Scan.mkv")).not.toBeInTheDocument();
    const handoffFileList = screen.getByLabelText("Scanned files").parentElement;
    expect(handoffFileList).toHaveAttribute("data-library-handoff", "true");
    expect(handoffFileList).toHaveClass("max-h-[20rem]", "shrink-0");
  });

  it("shows a slate placeholder and offers both Dashboard handoff scopes", async () => {
    const user = userEvent.setup();
    const rows = [auditRow(1, true), auditRow(2, false)];
    const files = rows.flatMap((row) => row.allFilePaths.map(media));
    const setFileSelection = vi.fn().mockImplementation((selectedPaths: string[]) => Promise.resolve({
      updatedUtc: "2026-08-22T20:00:00Z",
      files,
      selectedPaths,
      summary: { total: 4, mkv: 4, mp4: 0, failed: 0, cached: 0 }
    }));
    const getLibraryLocalArtwork = vi.fn().mockRejectedValue(new Error("No local poster"));
    const completedJob = {
      id: "scan-1",
      status: "Completed",
      completed: 4,
      total: 4,
      files,
      summary: { total: 4, mkv: 4, mp4: 0, failed: 0, cached: 0 },
      skipped: [],
      error: ""
    } as ScanJobResponse;

    renderWithBackend(
      <MediaLibraryProvider><LibraryPage /></MediaLibraryProvider>,
      {
        getWebSettings: () => Promise.resolve(settings()),
        getLibraryCatalog: () => Promise.resolve({ items: [{ id: "series-1", title: "Example Show", year: 2024, mediaType: "series", hasPoster: false }] }),
        getLibraryLocalArtwork,
        startScan: () => Promise.resolve({ ...completedJob, status: "Queued", files: [] }),
        getScanJob: () => Promise.resolve(completedJob),
        buildLibraryAudit: () => Promise.resolve({ summary: { groups: 2, files: 4, issueGroups: 1, standardGroups: 1 }, items: rows }),
        setFileSelection,
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-22T20:00:00Z", files, selectedPaths: [], summary: completedJob.summary })
      }
    );

    const card = await screen.findByRole("button", { name: /open example show library details/i });
    await waitFor(() => expect(getLibraryLocalArtwork).toHaveBeenCalledWith({
      folderPaths: [
        "/media/tv/Example Show/Season 1",
        "/media/tv/Example Show/Season 2"
      ]
    }));
    expect(screen.getByText(/no poster found/i)).toBeInTheDocument();
    expect(screen.getByText(/mismatch · 1/i)).toBeInTheDocument();

    await user.click(card);
    expect(await screen.findByRole("button", { name: /send mismatches \+ template to dashboard/i })).toBeEnabled();
    expect(screen.getByRole("button", { name: /send all files to dashboard/i })).toBeEnabled();
    await user.click(screen.getByRole("button", { name: /send mismatches \+ template to dashboard/i }));

    await waitFor(() => expect(setFileSelection).toHaveBeenCalledWith([
      "/media/tv/Example Show/Season 1/Episode 01.mkv",
      "/media/tv/Example Show/Season 1/Episode 02.mkv"
    ], true));
  });

  it("loads every library automatically and restores each one immediately", async () => {
    const user = userEvent.setup();
    const rowFor = (title: string, folder: string): LibraryAuditRow => {
      const base = `/media/${folder}/${title}/Season 1`;
      return {
        ...auditRow(1, false),
        folderPath: base,
        folderName: `${title} / Season 1`,
        templateFilePath: `${base}/Episode 01.mkv`,
        issueFilePaths: [],
        allFilePaths: [`${base}/Episode 01.mkv`, `${base}/Episode 02.mkv`]
      };
    };
    const alpha = rowFor("Alpha Show", "alpha");
    const beta = rowFor("Beta Show", "beta");
    const scanFiles = new Map<string, MediaFileRow[]>();
    let scanNumber = 0;
    const startScan = vi.fn().mockImplementation((request: { sources: string[] }) => {
      scanNumber += 1;
      const id = `scan-${scanNumber}`;
      const rows = [
        ...(request.sources.some((source) => source.includes("alpha")) ? [alpha] : []),
        ...(request.sources.some((source) => source.includes("beta")) ? [beta] : [])
      ];
      scanFiles.set(id, rows.flatMap((row) => row.allFilePaths.map(media)));
      return Promise.resolve({ id, status: "Queued", files: [], completed: 0, total: 0 } as ScanJobResponse);
    });
    const persistentSettings = {
      ...settings(),
      mediaServers: [{
        ...settings().mediaServers[0],
        id: "persistent-server",
        libraries: [
          { id: "alpha", name: "Alpha", type: "tvshows", serverPath: "/srv/alpha", containerPath: "/media/alpha", isEnabled: true },
          { id: "beta", name: "Beta", type: "tvshows", serverPath: "/srv/beta", containerPath: "/media/beta", isEnabled: true }
        ]
      }]
    } as WebSettings;

    renderWithBackend(
      <MediaLibraryProvider><LibraryPage /></MediaLibraryProvider>,
      {
        getWebSettings: () => Promise.resolve(persistentSettings),
        getLibraryCatalog: ({ libraryName }) => Promise.resolve({
          items: [{ id: libraryName, title: `${libraryName} Show`, year: 2024, mediaType: "series", hasPoster: false }]
        }),
        getLibraryLocalArtwork: () => Promise.reject(new Error("No local poster")),
        startScan,
        getScanJob: (id) => {
          const files = scanFiles.get(id) ?? [];
          return Promise.resolve({
            id,
            status: "Completed",
            completed: files.length,
            total: files.length,
            files,
            summary: { total: files.length, mkv: files.length, mp4: 0, failed: 0, cached: 0 },
            skipped: [],
            error: ""
          } as ScanJobResponse);
        },
        buildLibraryAudit: (files) => {
          const row = files[0]?.path.includes("/alpha/") ? alpha : beta;
          return Promise.resolve({
            summary: { groups: 1, files: files.length, issueGroups: 0, standardGroups: 1 },
            items: [row]
          });
        }
      }
    );

    expect(await screen.findByRole("button", { name: /open alpha show library details/i })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Beta", exact: true }));
    expect(await screen.findByRole("button", { name: /open beta show library details/i })).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: /rebuild selected library/i }));
    await waitFor(() => expect(startScan).toHaveBeenCalledTimes(2));
    expect(startScan).toHaveBeenLastCalledWith(expect.objectContaining({
      sources: ["/media/beta"],
      forceRefresh: false
    }));

    await user.click(screen.getByRole("button", { name: "Alpha", exact: true }));
    expect(screen.getByRole("button", { name: /open alpha show library details/i })).toBeInTheDocument();
    expect(startScan).toHaveBeenNthCalledWith(1, expect.objectContaining({
      sources: expect.arrayContaining(["/media/alpha", "/media/beta"]),
      forceRefresh: false
    }));
  });

  it("rebuilds only the right-clicked title and refreshes its cached audit", async () => {
    const user = userEvent.setup();
    const rows = [auditRow(1, false), auditRow(2, false)];
    const files = rows.flatMap((row) => row.allFilePaths.map(media));
    const startScan = vi.fn()
      .mockResolvedValueOnce({ id: "overview-scan", status: "Queued", files: [], completed: 0, total: 0 } as ScanJobResponse)
      .mockResolvedValueOnce({ id: "title-scan", status: "Queued", files: [], completed: 0, total: 0 } as ScanJobResponse);
    const buildLibraryAudit = vi.fn().mockImplementation((scannedFiles: MediaFileRow[]) => Promise.resolve({
      summary: { groups: rows.length, files: scannedFiles.length, issueGroups: 0, standardGroups: rows.length },
      items: rows
    }));
    const titleSettings = {
      ...settings(),
      mediaServers: [{
        ...settings().mediaServers[0],
        id: "title-rebuild-server",
        libraries: [{ id: "title-tv", name: "Title TV", type: "tvshows", serverPath: "/srv/tv", containerPath: "/media/tv", isEnabled: true }]
      }]
    } as WebSettings;

    renderWithBackend(
      <MediaLibraryProvider><LibraryPage /></MediaLibraryProvider>,
      {
        getWebSettings: () => Promise.resolve(titleSettings),
        getLibraryCatalog: () => Promise.resolve({ items: [{ id: "example", title: "Example Show", year: 2024, mediaType: "series", hasPoster: false }] }),
        getLibraryLocalArtwork: () => Promise.reject(new Error("No local poster")),
        startScan,
        getScanJob: (id) => Promise.resolve({
          id,
          status: "Completed",
          completed: files.length,
          total: files.length,
          files,
          summary: { total: files.length, mkv: files.length, mp4: 0, failed: 0, cached: 0 },
          skipped: [],
          error: ""
        } as ScanJobResponse),
        buildLibraryAudit
      }
    );

    const card = await screen.findByRole("button", { name: /open example show library details/i });
    fireEvent.contextMenu(card, { clientX: 120, clientY: 140 });
    await user.click(screen.getByRole("menuitem", { name: /rebuild this title/i }));

    await waitFor(() => expect(startScan).toHaveBeenCalledTimes(2));
    expect(startScan).toHaveBeenLastCalledWith({
      sources: ["/media/tv/Example Show"],
      ignoredFolderNames: [],
      forceRefresh: true,
      keepWorkingSet: true
    });
    await waitFor(() => expect(buildLibraryAudit).toHaveBeenCalledTimes(2));
    expect(await screen.findByText(/example show rebuilt: 4 files/i)).toBeInTheDocument();
  });

  function posterBackend(overrides: Partial<BackendClient> = {}): Partial<BackendClient> {
    const rows = [auditRow(1, false)];
    const files = rows.flatMap((row) => row.allFilePaths.map(media));
    const job = {
      id: "poster-scan",
      status: "Completed",
      completed: files.length,
      total: files.length,
      files,
      summary: { total: files.length, mkv: files.length, mp4: 0, failed: 0, cached: 0 },
      skipped: [],
      error: ""
    } as ScanJobResponse;
    return {
      getWebSettings: () => Promise.resolve(settings()),
      getLibraryCatalog: () => Promise.resolve({ items: [{ id: "series-1", title: "Example Show", year: 2024, mediaType: "series", hasPoster: true }] }),
      libraryArtworkUrl: ({ serverId, itemId }, version) => `/api/library/artwork/${serverId}/${itemId}?v=${version}`,
      startScan: () => Promise.resolve({ ...job, status: "Queued", files: [] }),
      getScanJob: () => Promise.resolve(job),
      buildLibraryAudit: () => Promise.resolve({ summary: { groups: 1, files: files.length, issueGroups: 0, standardGroups: 1 }, items: rows }),
      ...overrides
    };
  }

  it("loads a media-server poster as an image URL instead of inline data", async () => {
    const getLibraryArtwork = vi.fn();
    const getLibraryLocalArtwork = vi.fn();
    renderWithBackend(
      <MediaLibraryProvider><LibraryPage /></MediaLibraryProvider>,
      posterBackend({ getLibraryArtwork, getLibraryLocalArtwork })
    );

    const poster = await screen.findByRole("img", { name: "Example Show poster" });
    expect(poster).toHaveAttribute("src", "/api/library/artwork/server-1/series-1?v=1");
    expect(poster).toHaveAttribute("loading", "lazy");
    expect(getLibraryArtwork).not.toHaveBeenCalled();
    expect(getLibraryLocalArtwork).not.toHaveBeenCalled();
  });

  it("asks once why a poster failed and points a rejected key at Settings", async () => {
    const rejected = new ApiError("Jellyfin rejected the stored API key (HTTP 401). Enter the key again in Settings.", { code: "credential_rejected", status: 424 });
    const getLibraryArtwork = vi.fn().mockRejectedValue(rejected);
    const getLibraryLocalArtwork = vi.fn().mockRejectedValue(new Error("No local poster"));
    renderWithBackend(
      <MediaLibraryProvider><LibraryPage /></MediaLibraryProvider>,
      posterBackend({ getLibraryArtwork, getLibraryLocalArtwork })
    );

    fireEvent.error(await screen.findByRole("img", { name: "Example Show poster" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(/rejected the stored API key/i);
    expect(screen.getByRole("link", { name: /open settings/i })).toHaveAttribute("href", "/settings");
    await waitFor(() => expect(getLibraryLocalArtwork).toHaveBeenCalledTimes(1));
    expect(getLibraryArtwork).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("img", { name: "Example Show poster" })).not.toBeInTheDocument();
  });

  it("asks again after an answer that was not about the key", async () => {
    const show = (name: string): LibraryAuditRow => ({ ...auditRow(1, false), folderPath: `/media/tv/${name}/Season 1`, folderName: `${name} / Season 1`, allFilePaths: [`/media/tv/${name}/Season 1/Episode 01.mkv`] });
    const rows = [show("Alpha Show"), show("Beta Show"), show("Gamma Show")];
    let release: () => void = () => undefined;
    const getLibraryArtwork = vi.fn()
      .mockImplementationOnce(() => new Promise((_, reject) => { release = () => reject(new ApiError("Not found", { code: "not_found", status: 404 })); }))
      .mockRejectedValue(new ApiError("Jellyfin rejected the stored API key.", { code: "credential_rejected", status: 424 }));
    renderWithBackend(
      <MediaLibraryProvider><LibraryPage /></MediaLibraryProvider>,
      posterBackend({
        getLibraryArtwork,
        getLibraryLocalArtwork: () => Promise.reject(new Error("No local poster")),
        getLibraryCatalog: () => Promise.resolve({ items: [
          { id: "alpha", title: "Alpha Show", year: 2024, mediaType: "series", hasPoster: true },
          { id: "beta", title: "Beta Show", year: 2024, mediaType: "series", hasPoster: true },
          { id: "gamma", title: "Gamma Show", year: 2024, mediaType: "series", hasPoster: true }
        ] }),
        buildLibraryAudit: () => Promise.resolve({ summary: { groups: 3, files: 3, issueGroups: 0, standardGroups: 3 }, items: rows })
      })
    );

    fireEvent.error(await screen.findByRole("img", { name: "Alpha Show poster" }));
    fireEvent.error(screen.getByRole("img", { name: "Beta Show poster" }));
    expect(getLibraryArtwork).toHaveBeenCalledTimes(1);
    release();
    await waitFor(() => expect(getLibraryArtwork.mock.results[0].value).rejects.toThrow("Not found"));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    fireEvent.error(screen.getByRole("img", { name: "Gamma Show poster" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(/rejected the stored API key/i);
    expect(getLibraryArtwork).toHaveBeenCalledTimes(2);
  });

  it("requests no media-server posters while the catalog says the key is rejected", async () => {
    const libraryArtworkUrl = vi.fn(() => "/poster");
    renderWithBackend(
      <MediaLibraryProvider><LibraryPage /></MediaLibraryProvider>,
      posterBackend({
        libraryArtworkUrl,
        getLibraryCatalog: () => Promise.reject(new ApiError("Jellyfin rejected the stored API key.", { code: "credential_rejected", status: 424 })),
        getLibraryLocalArtwork: () => Promise.reject(new Error("No local poster"))
      })
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(/rejected the stored API key/i);
    expect(await screen.findByText(/no poster found/i)).toBeInTheDocument();
    expect(libraryArtworkUrl).not.toHaveBeenCalled();
  });

  it("sends a title to Dashboard with the backend's rows, not copies kept in the browser", async () => {
    const user = userEvent.setup();
    const backend = posterBackend({ getLibraryCatalog: () => Promise.resolve({ items: [] }), getLibraryLocalArtwork: () => Promise.reject(new Error("No local poster")) });
    const held = media("/media/tv/Example Show/Season 1/Episode 01.mkv");
    const setFileSelection = vi.fn().mockResolvedValue({ selectedPaths: [held.path] });
    renderWithBackend(
      <MediaLibraryProvider><LibraryPage /></MediaLibraryProvider>,
      {
        ...backend,
        setFileSelection,
        getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-22T20:00:00Z", files: [{ ...held, fileName: "Fresh from backend.mkv" }], selectedPaths: [], summary: { total: 1, mkv: 1, mp4: 0, failed: 0, cached: 0 } })
      }
    );

    await user.click(await screen.findByRole("button", { name: /open example show library details/i }));
    await user.click(screen.getByRole("button", { name: /send all files to dashboard/i }));

    await waitFor(() => expect(setFileSelection).toHaveBeenCalledWith([
      "/media/tv/Example Show/Season 1/Episode 01.mkv",
      "/media/tv/Example Show/Season 1/Episode 02.mkv"
    ], true));
    await waitFor(() => expect(setFileSelection).toHaveBeenLastCalledWith([held.path], true));
  });

  function openTitleWith(overrides: Partial<BackendClient>) {
    const backend = posterBackend({ getLibraryCatalog: () => Promise.resolve({ items: [] }), getLibraryLocalArtwork: () => Promise.reject(new Error("No local poster")), ...overrides });
    renderWithBackend(
      <MediaLibraryProvider>
        <Routes>
          <Route path="/" element={<LibraryPage />} />
          <Route path="/dashboard" element={<h1>Dashboard</h1>} />
        </Routes>
      </MediaLibraryProvider>,
      backend
    );
  }

  it("leaves Dashboard alone when none of the title's files can be read", async () => {
    const user = userEvent.setup();
    const setFileSelection = vi.fn().mockResolvedValue({ selectedPaths: ["/media/other.mkv"] });
    openTitleWith({
      setFileSelection,
      getCurrentScanFiles: () => Promise.resolve({ updatedUtc: "2026-08-22T20:00:00Z", files: [media("/media/other.mkv")], selectedPaths: ["/media/other.mkv"], summary: { total: 1, mkv: 1, mp4: 0, failed: 0, cached: 0 } })
    });

    await user.click(await screen.findByRole("button", { name: /open example show library details/i }));
    await user.click(screen.getByRole("button", { name: /send all files to dashboard/i }));

    const dialog = screen.getByRole("dialog");
    expect(await within(dialog).findByText(/none of this title's files could be read/i)).toBeInTheDocument();
    expect(setFileSelection).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("heading", { name: "Dashboard" })).not.toBeInTheDocument();
  });

  it("says so in the dialog when the sent files cannot be loaded back", async () => {
    const user = userEvent.setup();
    openTitleWith({
      setFileSelection: () => Promise.resolve({ selectedPaths: [] }),
      getCurrentScanFiles: () => Promise.reject(new Error("server went away"))
    });

    await user.click(await screen.findByRole("button", { name: /open example show library details/i }));
    await user.click(screen.getByRole("button", { name: /send all files to dashboard/i }));

    expect(await within(screen.getByRole("dialog")).findByText(/server went away/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /send all files to dashboard/i })).toBeEnabled();
  });

  it("runs one hand-off at a time and shows its progress in the dialog", async () => {
    const user = userEvent.setup();
    let release: (value: { selectedPaths: string[] }) => void = () => undefined;
    const setFileSelection = vi.fn().mockImplementation(() => new Promise((resolve) => { release = resolve; }));
    openTitleWith({ setFileSelection, getCurrentScanFiles: () => new Promise(() => undefined) });

    await user.click(await screen.findByRole("button", { name: /open example show library details/i }));
    const sendAll = screen.getByRole("button", { name: /send all files to dashboard/i });
    await user.dblClick(sendAll);

    expect(within(screen.getByRole("dialog")).getByText(/sending example show to dashboard/i)).toBeInTheDocument();
    expect(sendAll).toBeDisabled();
    expect(screen.getByRole("button", { name: /send mismatches \+ template to dashboard/i })).toBeDisabled();
    expect(setFileSelection).toHaveBeenCalledTimes(1);
    release({ selectedPaths: [] });
  });

  it("does not save an empty library when the scan's files are no longer held", async () => {
    const buildLibraryAudit = vi.fn();
    renderWithBackend(
      <MediaLibraryProvider><LibraryPage /></MediaLibraryProvider>,
      {
        getWebSettings: () => Promise.resolve(settings()),
        getLibraryCatalog: () => Promise.resolve({ items: [] }),
        startScan: () => Promise.resolve({ id: "lost-scan", status: "Queued", files: [], completed: 0, total: 0 } as ScanJobResponse),
        getScanJob: (id) => Promise.resolve({
          id,
          status: "Completed",
          completed: 4,
          total: 4,
          files: [],
          summary: { total: 4, mkv: 4, mp4: 0, failed: 0, cached: 0 },
          skipped: [],
          error: ""
        } as ScanJobResponse),
        buildLibraryAudit
      }
    );

    expect(await screen.findByText(/scan results are no longer held/i)).toBeInTheDocument();
    expect(buildLibraryAudit).not.toHaveBeenCalled();
  });
});
