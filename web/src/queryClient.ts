import { QueryClient } from "@tanstack/react-query";
import { ApiError } from "./backend/error";

// A 4xx is the server's answer to this request; asking again cannot change it.
// A timeout is not repeated either: the request may still be running.
function retry(failureCount: number, error: Error): boolean {
  if (error instanceof ApiError) {
    if (error.code === "TIMEOUT") return false;
    if (error.status !== undefined && error.status >= 400 && error.status < 500) return false;
  }
  return failureCount < 1;
}

export function createQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: {
        refetchOnWindowFocus: false,
        retry
      }
    }
  });
}
