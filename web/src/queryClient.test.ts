import { expect, it, vi } from "vitest";
import { ApiError } from "./backend/error";
import { createQueryClient } from "./queryClient";

function attempts(error: Error) {
  const queryFn = vi.fn().mockRejectedValue(error);
  return createQueryClient()
    .fetchQuery({ queryKey: ["probe"], queryFn, retryDelay: 0 })
    .catch(() => queryFn.mock.calls.length);
}

it("does not repeat a request the server rejected with a 4xx", async () => {
  expect(await attempts(new ApiError("Not found", { status: 404 }))).toBe(1);
  expect(await attempts(new ApiError("Authentication required.", { status: 401 }))).toBe(1);
});

it("retries a server or network failure once", async () => {
  expect(await attempts(new ApiError("Unavailable", { status: 503 }))).toBe(2);
  expect(await attempts(new ApiError("Unable to reach the MKVO server.", { code: "TRANSPORT_ERROR" }))).toBe(2);
});

it("does not send a timed-out request again", async () => {
  expect(await attempts(new ApiError("The MKVO server took too long to answer.", { code: "TIMEOUT" }))).toBe(1);
});
