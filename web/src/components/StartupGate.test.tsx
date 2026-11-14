import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import StartupGate from "./StartupGate";
import { getStatus } from "../api";
import { ApiError } from "../backend/error";

vi.mock("../api", () => ({ getStatus: vi.fn() }));

const status = { name: "MKVO", version: "test", mediaRoot: "/media", configRoot: "/config", sourceRoots: [], tools: [], contractVersion: 1 };

describe("StartupGate", () => {
  afterEach(cleanup);
  beforeEach(() => vi.mocked(getStatus).mockReset());

  it("shows the app once the desktop backend has started", async () => {
    vi.mocked(getStatus).mockResolvedValue(status);
    render(<StartupGate><div>Desktop app</div></StartupGate>);
    expect(screen.queryByText("Desktop app")).not.toBeInTheDocument();
    expect(await screen.findByText("Desktop app")).toBeInTheDocument();
  });

  it("blocks the app with the startup error instead of an empty shell", async () => {
    vi.mocked(getStatus).mockRejectedValueOnce(new ApiError("legacy data migration failed: disk full", { code: "internal" }));
    vi.mocked(getStatus).mockResolvedValue(status);
    render(<StartupGate><div>Desktop app</div></StartupGate>);
    expect(await screen.findByRole("alert")).toHaveTextContent("legacy data migration failed: disk full");
    expect(screen.queryByText("Desktop app")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry" }));
    expect(await screen.findByText("Desktop app")).toBeInTheDocument();
  });
});
