import { ApiError } from "../backend/error";

export type AuthSession = {
  username?: string;
  authenticated: boolean;
  password_required?: boolean;
  idle_timeout_minutes?: number | null;
};

export type SecuritySettings = {
  idle_timeout_minutes: number | null;
  local_network_bypass: boolean;
};

export function announceUnauthorized() {
  window.dispatchEvent(new Event("mkvo:unauthorized"));
}

async function request<T>(path: string, method = "GET", body?: unknown): Promise<T> {
  let response: Response;
  try {
    response = await fetch(path, {
      method, credentials: "same-origin", cache: "no-store", signal: AbortSignal.timeout(30_000),
      headers: body === undefined ? undefined : { "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body)
    });
  } catch (cause) {
    throw new ApiError("Unable to reach the MKVO server.", { code: "TRANSPORT_ERROR", cause });
  }
  if (response.status === 401) announceUnauthorized();
  if (response.status === 204) return undefined as T;
  // A proxy in front of MKVO answers with an HTML error page, not JSON.
  const payload = await response.json().catch(() => undefined);
  if (!response.ok || payload === undefined) {
    throw new ApiError(payload?.detail ?? `Unexpected response from the MKVO server (HTTP ${response.status}).`, { status: response.status });
  }
  return payload as T;
}

export const api = {
  authStatus: () => request<AuthSession>("/api/auth/status"),
  authLogin: (username: string, password: string) => request<AuthSession>("/api/auth/login", "POST", { username, password }),
  authLogout: () => request<AuthSession>("/api/auth/logout", "POST"),
  authActivity: () => request<void>("/api/auth/activity", "POST"),
  securitySettings: () => request<SecuritySettings>("/api/security/settings"),
  saveSecuritySettings: (settings: SecuritySettings) => request<SecuritySettings>("/api/security/settings", "PUT", settings)
};
