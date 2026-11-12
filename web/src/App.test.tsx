import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { expect, it } from "vitest";
import App from "./App";
import { createMockBackendClient } from "./backend/mockClient";
import { setBackendClient } from "./backend/runtime";

it("loads a route's page chunk inside the layout", async () => {
  // The desktop transport has no sign-in gate in front of the routes.
  setBackendClient(createMockBackendClient({ transport: "tauri", getOperationLogs: () => Promise.resolve({ entries: [] }) }));
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <MemoryRouter initialEntries={["/logs"]}><App /></MemoryRouter>
    </QueryClientProvider>
  );

  expect(await screen.findByRole("heading", { name: "Logs" })).toBeInTheDocument();
  expect(screen.getByRole("link", { name: /Settings/ })).toBeInTheDocument();
});
