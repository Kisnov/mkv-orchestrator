import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { expect, it, vi } from "vitest";
import App from "./App";
import { createMockBackendClient } from "./backend/mockClient";
import { setBackendClient } from "./backend/runtime";

// Stands in for a chunk whose hash is gone after an update.
vi.mock("./pages/SettingsPage", () => { throw new Error("Failed to fetch dynamically imported module"); });

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

it("offers a reload inside the layout when a page chunk cannot be loaded", async () => {
  vi.spyOn(console, "error").mockImplementation(() => undefined);
  setBackendClient(createMockBackendClient({ transport: "tauri", getOperationLogs: () => Promise.resolve({ entries: [] }) }));
  render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
      <MemoryRouter initialEntries={["/settings"]}><App /></MemoryRouter>
    </QueryClientProvider>
  );

  expect(await screen.findByRole("button", { name: "Reload" })).toBeInTheDocument();
  expect(screen.getByRole("alert")).toHaveTextContent("This page could not be loaded");
  // The sidebar survives, and another page still opens from it.
  fireEvent.click(screen.getByRole("link", { name: /Logs/ }));
  expect(await screen.findByRole("heading", { name: "Logs" })).toBeInTheDocument();
});
