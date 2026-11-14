import { useState } from "react";
import userEvent from "@testing-library/user-event";
import { screen, waitFor } from "@testing-library/react";
import { Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { Layout } from "./Layout";
import { MediaLibraryProvider } from "../state/MediaLibraryContext";
import { renderWithBackend } from "../test/render";

// The overlay hook keeps its own observer; these tests only count Layout's.
vi.mock("./useOverlayAccessibility", () => ({ useOverlayAccessibility: () => undefined }));

beforeEach(() => {
  window.localStorage.clear();
  window.sessionStorage.clear();
});
afterEach(() => vi.unstubAllGlobals());

function Rows() {
  const [rows, setRows] = useState(["Episode 1.mkv"]);
  return (
    <>
      <button type="button" onClick={() => setRows([...rows, "Episode 2.mkv"])}>Add row</button>
      <table>
        <thead><tr><th>File</th><th>Status</th></tr></thead>
        <tbody>{rows.map((row) => <tr key={row}><td>{row}</td><td>Scanned</td></tr>)}</tbody>
      </table>
    </>
  );
}

function renderLayout(mobile: boolean) {
  vi.stubGlobal("matchMedia", () => ({ matches: mobile, addEventListener: () => undefined, removeEventListener: () => undefined }));
  const observe = vi.spyOn(MutationObserver.prototype, "observe");
  renderWithBackend(<MediaLibraryProvider><Routes><Route element={<Layout />}><Route path="*" element={<Rows />} /></Route></Routes></MediaLibraryProvider>, {
    getStatus: () => Promise.resolve({ name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 })
  });
  // Read before any query runs: Testing Library waits with observers of its own.
  return observe.mock.calls.map(([target]) => target);
}

it("leaves tables alone and observes nothing on a desktop viewport", () => {
  const observed = renderLayout(false);

  expect(observed).toEqual([]);
  expect(screen.getByRole("table")).not.toHaveClass("mobile-data-table");
  expect(screen.getByText("Episode 1.mkv")).not.toHaveAttribute("data-label");
});

it("labels table cells on a mobile viewport by observing only the page content", async () => {
  const user = userEvent.setup();
  const observed = renderLayout(true);

  expect(observed).toEqual([document.querySelector(".page-content")]);
  const table = screen.getByRole("table");
  expect(table).toHaveClass("mobile-data-table");
  expect(table).toHaveAttribute("role", "table");
  expect(screen.getByText("Episode 1.mkv")).toHaveAttribute("data-label", "File");
  expect(screen.getAllByText("Scanned")[0]).toHaveAttribute("data-label", "Status");

  await user.click(screen.getByRole("button", { name: "Add row" }));
  await waitFor(() => expect(screen.getByText("Episode 2.mkv")).toHaveAttribute("data-label", "File"));
});
