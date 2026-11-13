import type { LogExport } from "./client";
import type {
  AppStatus,
  CurrentScanResponse,
  FileSelectionResponse,
  FileSystemResponse,
  LibraryArtworkRequest,
  LibraryArtworkResponse,
  LibraryAuditResponse,
  LibraryCatalogRequest,
  LibraryCatalogResponse,
  LibraryLocalArtworkRequest,
  MediaFileRow,
  MediaServerSyncResponse,
  MediaServerTestResponse,
  MuxPreviewRequest,
  MuxPreviewResponse,
  OperationJobResponse,
  OperationLogEntry,
  PropEditPreviewRequest,
  PropEditPreviewResponse,
  PropEditTemplateResponse,
  RenameApplyResponse,
  RenameBatchListResponse,
  RenameBatchUndoPreviewResponse,
  RenameBatchUndoResponse,
  RenamePreviewResponse,
  RenameProviderTestResponse,
  RenameScopeRow,
  RenameSearchResult,
  ScanJobResponse,
  ScanRequest,
  WebSettings,
  WebSettingsRequest
} from "../api";
import type {
  BackendClient,
  JobProgressEvent,
  JobProgressSubscription,
  MediaServerConnectionRequest,
  PropEditTemplateRequest,
  RenameApplyRequest,
  RenamePreviewRequest,
  RenameProviderTestRequest,
  RenameScopesRequest,
  RenameSearchRequest,
  Unsubscribe
} from "./client";
import { ApiError, normalizeApiError } from "./error";
import { announceUnauthorized } from "../auth/api";
import { validateContract, type ContractName } from "../generated/contracts";

export type HttpBackendClientOptions = {
  baseUrl?: string;
  fetch?: typeof fetch;
};

type ErrorPayload = {
  message?: unknown;
  error?: unknown;
  code?: unknown;
  details?: unknown;
  title?: unknown;
  detail?: unknown;
};

const TERMINAL_JOB_STATUSES = new Set(["Completed", "Failed", "Skipped", "Canceled"]);
const REQUEST_TIMEOUT_MS = 60_000;
// Calls the server answers only after doing the work, and posters, which
// queue behind the browser's per-host connection limit.
const LONG_REQUEST_TIMEOUT_MS = 30 * 60_000;

function resolveErrorMessage(payload: ErrorPayload | undefined, fallback: string): string {
  if (typeof payload?.message === "string" && payload.message.trim()) {
    return payload.message;
  }
  if (typeof payload?.error === "string" && payload.error.trim()) {
    return payload.error;
  }
  if (typeof payload?.detail === "string" && payload.detail.trim()) {
    return payload.detail;
  }
  if (typeof payload?.title === "string" && payload.title.trim()) {
    return payload.title;
  }
  if (payload?.error && typeof payload.error === "object") {
    const nestedMessage = (payload.error as ErrorPayload).message;
    if (typeof nestedMessage === "string" && nestedMessage.trim()) {
      return nestedMessage;
    }
  }
  return fallback;
}

export class HttpBackendClient implements BackendClient {
  readonly transport = "http" as const;

  private readonly baseUrl: string;
  private readonly fetchImpl: typeof fetch;

  constructor(options: HttpBackendClientOptions = {}) {
    this.baseUrl = (options.baseUrl ?? "").replace(/\/+$/, "");
    this.fetchImpl = options.fetch ?? ((input, init) => fetch(input, init));
  }

  private url(path: string): string {
    return `${this.baseUrl}${path}`;
  }

  private async request<T>(
    path: string,
    init?: RequestInit,
    contract?: ContractName,
    timeoutMs = REQUEST_TIMEOUT_MS
  ): Promise<T> {
    let response: Response;
    let text: string;
    try {
      const headers = new Headers(init?.headers);
      if (!headers.has("Accept")) {
        headers.set("Accept", "application/json");
      }
      response = await this.fetchImpl(this.url(path), {
        credentials: "same-origin",
        signal: AbortSignal.timeout(timeoutMs),
        ...init,
        headers
      });
      text = await response.text();
    } catch (error) {
      if ((error as { name?: unknown } | null)?.name === "TimeoutError") {
        throw new ApiError("The MKVO server took too long to answer.", { code: "TIMEOUT", cause: error });
      }
      throw normalizeApiError(error, `Unable to reach the MKVO server at ${this.url(path)}.`);
    }

    let payload: unknown;
    if (text) {
      try {
        payload = JSON.parse(text);
      } catch {
        payload = text;
      }
    }

    if (!response.ok) {
      if (response.status === 401) announceUnauthorized();
      const errorPayload = payload && typeof payload === "object" ? (payload as ErrorPayload) : undefined;
      const fallback =
        (typeof payload === "string" && payload.trim()) ||
        `${response.status} ${response.statusText || "Request failed"}`;
      const code = typeof errorPayload?.code === "string" ? errorPayload.code : `HTTP_${response.status}`;
      throw new ApiError(resolveErrorMessage(errorPayload, fallback), {
        code,
        status: response.status,
        details: errorPayload?.details ?? errorPayload?.detail ?? payload
      });
    }

    if (contract) {
      const validation = validateContract(contract, payload);
      if (!validation.ok) {
        throw new ApiError(`The server returned an invalid ${contract} response.`, {
          code: "INVALID_RESPONSE",
          details: validation.errors
        });
      }
    }
    return payload as T;
  }

  private json<T>(
    path: string,
    method: string,
    body?: unknown,
    contract?: ContractName,
    timeoutMs?: number
  ): Promise<T> {
    return this.request<T>(path, {
      method,
      headers: body === undefined ? undefined : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body)
    }, contract, timeoutMs);
  }

  getStatus(): Promise<AppStatus> {
    return this.request<AppStatus>("/api/status", undefined, "AppStatus");
  }

  browseFileSystem(path?: string): Promise<FileSystemResponse> {
    const query = path ? `?path=${encodeURIComponent(path)}` : "";
    return this.request<FileSystemResponse>(`/api/filesystem${query}`, undefined, "FileSystemResponse");
  }

  startScan(request: ScanRequest): Promise<ScanJobResponse> {
    return this.json<ScanJobResponse>("/api/scans", "POST", request, "ScanJobResponse");
  }

  getScanJob(id: string): Promise<ScanJobResponse> {
    return this.request<ScanJobResponse>(`/api/scans/${encodeURIComponent(id)}`, undefined, "ScanJobResponse");
  }

  cancelScan(id: string): Promise<ScanJobResponse> {
    return this.json<ScanJobResponse>(`/api/scans/${encodeURIComponent(id)}/cancel`, "POST", undefined, "ScanJobResponse");
  }

  getCurrentScanFiles(): Promise<CurrentScanResponse> {
    return this.request<CurrentScanResponse>("/api/files/current", undefined, "CurrentScanResponse");
  }

  clearCurrentScanFiles(): Promise<CurrentScanResponse> {
    return this.json<CurrentScanResponse>("/api/files/current", "DELETE", undefined, "CurrentScanResponse");
  }

  setFileSelection(paths: string[], replaceWorkingSet?: boolean): Promise<FileSelectionResponse> {
    // Replacing the working set may read files the backend has not cached.
    const timeout = replaceWorkingSet ? LONG_REQUEST_TIMEOUT_MS : undefined;
    return this.json<FileSelectionResponse>("/api/files/selection", "PUT", { paths, replaceWorkingSet }, "FileSelectionResponse", timeout);
  }

  // The server only ever lists inside its configured roots, so anything the
  // browser showed is already authorized and there is nothing to grant.
  authorizeBrowsedRoot(): Promise<void> {
    return Promise.resolve();
  }

  getWebSettings(): Promise<WebSettings> {
    return this.request<WebSettings>("/api/settings", undefined, "WebSettings");
  }

  saveWebSettings(request: WebSettingsRequest): Promise<WebSettings> {
    return this.json<WebSettings>("/api/settings", "PUT", request, "WebSettings");
  }

  testMediaServerConnection(request: MediaServerConnectionRequest): Promise<MediaServerTestResponse> {
    return this.json<MediaServerTestResponse>("/api/media-servers/test", "POST", request, "MediaServerTestResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  syncMediaServerLibraries(id: string): Promise<MediaServerSyncResponse> {
    return this.json<MediaServerSyncResponse>(`/api/media-servers/${encodeURIComponent(id)}/sync`, "POST", undefined, "MediaServerSyncResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  searchRenameMetadata(request: RenameSearchRequest): Promise<{ results: RenameSearchResult[] }> {
    return this.json<{ results: RenameSearchResult[] }>("/api/rename/search", "POST", request, "RenameSearchResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  loadRenameScopes(request: RenameScopesRequest): Promise<{ scopes: RenameScopeRow[] }> {
    return this.json<{ scopes: RenameScopeRow[] }>("/api/rename/scopes", "POST", request, "RenameScopesResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  testRenameProvider(request: RenameProviderTestRequest): Promise<RenameProviderTestResponse> {
    return this.json<RenameProviderTestResponse>("/api/rename/test-provider", "POST", request, "RenameProviderTestResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  buildRenamePreview(request: RenamePreviewRequest): Promise<RenamePreviewResponse> {
    return this.json<RenamePreviewResponse>("/api/rename/preview", "POST", request, "RenamePreviewResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  applyRenamePreview(request: RenameApplyRequest): Promise<RenameApplyResponse> {
    return this.json<RenameApplyResponse>("/api/rename/apply", "POST", request, "RenameApplyResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  getRenameBatches(): Promise<RenameBatchListResponse> {
    return this.request<RenameBatchListResponse>("/api/rename/batches", undefined, "RenameBatchListResponse");
  }

  previewRenameBatchUndo(id: string): Promise<RenameBatchUndoPreviewResponse> {
    return this.request<RenameBatchUndoPreviewResponse>(`/api/rename/batches/${encodeURIComponent(id)}/preview`, undefined, "RenameBatchUndoPreviewResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  undoRenameBatch(id: string): Promise<RenameBatchUndoResponse> {
    return this.json<RenameBatchUndoResponse>(`/api/rename/batches/${encodeURIComponent(id)}/undo`, "POST", undefined, "RenameBatchUndoResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  clearRenameBatches(): Promise<RenameBatchListResponse> {
    return this.json<RenameBatchListResponse>("/api/rename/batches", "DELETE", undefined, "RenameBatchListResponse");
  }

  buildMuxPreview(request: MuxPreviewRequest): Promise<MuxPreviewResponse> {
    return this.json<MuxPreviewResponse>("/api/mux/preview", "POST", request, "MuxPreviewResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  startMuxApply(request: MuxPreviewRequest): Promise<OperationJobResponse> {
    return this.json<OperationJobResponse>("/api/mux/apply", "POST", request, "OperationJobResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  getOperationJob(id: string): Promise<OperationJobResponse> {
    return this.request<OperationJobResponse>(`/api/operations/${encodeURIComponent(id)}`, undefined, "OperationJobResponse");
  }

  cancelOperationJob(id: string): Promise<OperationJobResponse> {
    return this.json<OperationJobResponse>(`/api/operations/${encodeURIComponent(id)}/cancel`, "POST", undefined, "OperationJobResponse");
  }

  loadPropEditTemplate(request: PropEditTemplateRequest): Promise<PropEditTemplateResponse> {
    return this.json<PropEditTemplateResponse>("/api/propedit/template", "POST", request, "PropEditTemplateResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  buildPropEditPreview(request: PropEditPreviewRequest): Promise<PropEditPreviewResponse> {
    return this.json<PropEditPreviewResponse>("/api/propedit/preview", "POST", request, "PropEditPreviewResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  startPropEditApply(request: PropEditPreviewRequest): Promise<OperationJobResponse> {
    return this.json<OperationJobResponse>("/api/propedit/apply", "POST", request, "OperationJobResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  buildLibraryAudit(files: MediaFileRow[]): Promise<LibraryAuditResponse> {
    return this.json<LibraryAuditResponse>(
      "/api/library/audit",
      "POST",
      { paths: files.map((file) => file.path) },
      "LibraryAuditResponse",
      LONG_REQUEST_TIMEOUT_MS
    );
  }

  getLibraryCatalog(request: LibraryCatalogRequest): Promise<LibraryCatalogResponse> {
    return this.json<LibraryCatalogResponse>("/api/library/catalog", "POST", request, "LibraryCatalogResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  getLibraryArtwork(request: LibraryArtworkRequest): Promise<LibraryArtworkResponse> {
    return this.json<LibraryArtworkResponse>("/api/library/artwork", "POST", request, "LibraryArtworkResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  getLibraryLocalArtwork(request: LibraryLocalArtworkRequest): Promise<LibraryArtworkResponse> {
    return this.json<LibraryArtworkResponse>("/api/library/local-artwork", "POST", request, "LibraryArtworkResponse", LONG_REQUEST_TIMEOUT_MS);
  }

  getOperationLogs(): Promise<{ entries: OperationLogEntry[] }> {
    return this.request<{ entries: OperationLogEntry[] }>("/api/logs", undefined, "OperationLogResponse");
  }

  clearOperationLogs(): Promise<{ entries: OperationLogEntry[] }> {
    return this.json<{ entries: OperationLogEntry[] }>("/api/logs", "DELETE", undefined, "OperationLogResponse");
  }

  exportOperationLogs(): Promise<LogExport> {
    return this.request<LogExport>("/api/logs/export");
  }

  async subscribeJobProgress(
    subscription: JobProgressSubscription,
    listener: (event: JobProgressEvent) => void,
    onError?: (error: ApiError) => void
  ): Promise<Unsubscribe> {
    let stopped = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const interval = Math.max(250, subscription.pollIntervalMs ?? 1000);

    const poll = async (): Promise<void> => {
      if (stopped) {
        return;
      }

      try {
        if (subscription.kind === "scan") {
          const job = await this.getScanJob(subscription.jobId);
          listener({ jobId: subscription.jobId, kind: "scan", job });
          if (TERMINAL_JOB_STATUSES.has(job.status)) {
            stopped = true;
            return;
          }
        } else {
          const job = await this.getOperationJob(subscription.jobId);
          listener({ jobId: subscription.jobId, kind: "operation", job });
          if (TERMINAL_JOB_STATUSES.has(job.status)) {
            stopped = true;
            return;
          }
        }
      } catch (error) {
        const apiError = normalizeApiError(error);
        onError?.(apiError);
        if (apiError.status === 401) {
          stopped = true;
          return;
        }
      }

      if (!stopped) {
        timer = setTimeout(() => void poll(), interval);
      }
    };

    void poll();
    return () => {
      stopped = true;
      if (timer !== undefined) {
        clearTimeout(timer);
      }
    };
  }
}
