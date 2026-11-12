import { QueryClient } from "@tanstack/react-query";
import { ApiError } from "./backend/error";

// A 4xx is the server's answer to this request; asking again cannot change it.
function retry(failureCount: number, error: Error): boolean {
  const status = error instanceof ApiError ? error.status : undefined;
  if (status !== undefined && status >= 400 && status < 500) return false;
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
