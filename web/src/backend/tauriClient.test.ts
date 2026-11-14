import { beforeEach, expect, it, vi } from "vitest";
import { TauriBackendClient } from "./tauriClient";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) as unknown }));

beforeEach(() => invoke.mockReset());

it("lists recent jobs through the desktop command", async () => {
  invoke.mockResolvedValue({ jobs: [] });

  await expect(new TauriBackendClient().listRecentJobs(20)).resolves.toEqual({ jobs: [] });
  expect(invoke).toHaveBeenCalledWith("list_recent_jobs", { limit: 20 });
});

it("reads watch health through the desktop command and validates it", async () => {
  const health = { running: false, backend: "native", watchedRoots: 0 };
  invoke.mockResolvedValueOnce(health).mockResolvedValueOnce({ running: false });
  const client = new TauriBackendClient();

  await expect(client.getWatchHealth()).resolves.toEqual(health);
  expect(invoke).toHaveBeenCalledWith("get_watch_health", undefined);
  await expect(client.getWatchHealth()).rejects.toMatchObject({ code: "INVALID_RESPONSE" });
});
