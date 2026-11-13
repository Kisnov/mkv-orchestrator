import { useState } from "react";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { MediaFileRow } from "../api";
import { MediaLibraryProvider } from "../state/MediaLibraryContext";
import { useFileSelection, useLibrarySelection } from "../state/useFileSelection";
import { renderWithBackend } from "../test/render";
import { fileColumns, type FileColumn } from "./fileColumns";
import { FileSelectionTable } from "./FileSelectionTable";

function file(name: string, audioLanguage = "cat"): MediaFileRow {
  return {
    path: `/media/${name}`, fileName: name, extension: ".mkv", status: "Scanned", reader: "mkvmerge",
    codec: "HEVC", resolution: "1920x1080", bitDepth: "10", hdr: "None", videoSummary: "HEVC",
    audioSummary: "x1", subtitleSummary: "None", attachmentSummary: "None", attachments: [],
    tracks: [
      { id: 0, trackNumber: 1, type: "video", codec: "HEVC", language: "und", name: "", channels: null, default: true, forced: false },
      { id: 1, trackNumber: 2, type: "audio", codec: "AAC", language: audioLanguage, name: "", channels: 2, default: true, forced: false }
    ]
  };
}

// 01-03 are identical, 04-06 carry a different audio language.
const files = [file("01.mkv"), file("02.mkv"), file("03.mkv"), file("04.mkv", "jpn"), file("05.mkv", "jpn"), file("06.mkv", "jpn")];

function Harness({ initialFiles = files, initialTemplatePath = initialFiles[0].path }: { initialFiles?: MediaFileRow[]; initialTemplatePath?: string }) {
  const [templatePath, setTemplatePath] = useState(initialTemplatePath);
  const [scanned, setScanned] = useState(initialFiles);
  const templateFile = scanned.find((item) => item.path === templatePath) ?? null;
  const selection = useFileSelection(scanned, templateFile);
  return (
    <>
      <button type="button" onClick={() => setTemplatePath("/media/04.mkv")}>Use 04 as template</button>
      <button type="button" onClick={() => setTemplatePath("/media/01.mkv")}>Use 01 as template</button>
      <button type="button" onClick={() => setScanned((current) => current.map((item) => ({ ...item })))}>Refresh files</button>
      <button type="button" onClick={() => setScanned((current) => [...current, file("07.mkv", "jpn")])}>Add a differing file</button>
      <output>{selection.selectedPaths.join(",")}</output>
      <span aria-label="hidden">{selection.selectedPaths.filter(selection.isHidden).length}</span>
      <FileSelectionTable title="Files" files={scanned} templateFile={templateFile} selection={selection} warnDiffering tintRows />
    </>
  );
}

function checkedNames() {
  return screen.getAllByRole("checkbox").filter((box) => (box as HTMLInputElement).checked).map((box) => box.getAttribute("aria-label"));
}

function box(name: string) {
  return screen.getByRole("checkbox", { name: `Select ${name}` });
}

describe("FileSelectionTable", () => {
  it("starts with the template and the files equal to it, and says how many differ", () => {
    render(<Harness />);

    expect(checkedNames()).toEqual(["Select 01.mkv", "Select 02.mkv", "Select 03.mkv"]);
    expect(screen.getByText("3 files differ from the template and are not selected.")).toBeInTheDocument();
    expect(screen.getByText("3 of 6 selected")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("/media/01.mkv,/media/02.mkv,/media/03.mkv");
  });

  it("shows the Dashboard status for each row and lists the differences on demand", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    const row = (name: string) => box(name).closest("tr")!;
    expect(within(row("01.mkv")).getByText("Template")).toBeInTheDocument();
    expect(within(row("02.mkv")).getByText("Scanned")).toBeInTheDocument();
    const warning = within(row("04.mkv")).getByRole("button", { name: "Warning (1)" });
    expect(warning).toHaveAttribute("aria-expanded", "false");

    await user.click(warning);
    expect(warning).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("Audio track · ID 1 language:").closest("li")).toHaveTextContent("Audio track · ID 1 language: jpn (template: cat)");
  });

  it("selects all, none, the inverse, and only the files equal to the template", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Select all" }));
    expect(checkedNames()).toHaveLength(6);
    expect(screen.getByText(/3 selected files differ from the template/)).toBeInTheDocument();
    expect(screen.queryByText(/are not selected/)).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Select none" }));
    expect(checkedNames()).toEqual([]);

    await user.click(box("02.mkv"));
    await user.click(screen.getByRole("button", { name: "Invert" }));
    expect(checkedNames()).toEqual(["Select 01.mkv", "Select 03.mkv", "Select 04.mkv", "Select 05.mkv", "Select 06.mkv"]);

    await user.click(screen.getByRole("button", { name: "Only equal to template" }));
    expect(checkedNames()).toEqual(["Select 01.mkv", "Select 02.mkv", "Select 03.mkv"]);
  });

  it("extends the last toggle over a Shift+click range, on the checkbox or on the name", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(box("04.mkv"));
    await user.keyboard("{Shift>}");
    await user.click(box("06.mkv"));
    await user.keyboard("{/Shift}");
    expect(checkedNames()).toEqual(["Select 01.mkv", "Select 02.mkv", "Select 03.mkv", "Select 04.mkv", "Select 05.mkv", "Select 06.mkv"]);

    await user.click(box("02.mkv"));
    await user.keyboard("{Shift>}");
    await user.click(screen.getByText("05.mkv"));
    await user.keyboard("{/Shift}");
    expect(checkedNames()).toEqual(["Select 01.mkv", "Select 06.mkv"]);

    // Without Shift the name toggles just its own row.
    await user.click(screen.getByText("03.mkv"));
    expect(checkedNames()).toEqual(["Select 01.mkv", "Select 03.mkv", "Select 06.mkv"]);
  });

  it("uses the displayed order for a range after sorting", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(screen.getByRole("button", { name: "Select none" }));
    await user.click(screen.getByRole("button", { name: "Sort by File" }));
    expect(screen.getAllByRole("checkbox").map((item) => item.getAttribute("aria-label"))[0]).toBe("Select 06.mkv");
    await user.click(box("06.mkv"));
    await user.keyboard("{Shift>}");
    await user.click(box("05.mkv"));
    await user.keyboard("{/Shift}");
    expect(checkedNames()).toEqual(["Select 06.mkv", "Select 05.mkv"]);
  });

  it("can be operated from the keyboard", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    box("04.mkv").focus();
    await user.keyboard(" ");
    expect(box("04.mkv")).toBeChecked();
    await user.tab();
    await user.tab();
    expect(box("05.mkv")).toHaveFocus();
    await user.keyboard(" ");
    expect(box("05.mkv")).toBeChecked();
  });

  it("recomputes the default when the template changes, dropping manual choices", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(box("02.mkv"));
    await user.click(box("06.mkv"));
    expect(checkedNames()).toEqual(["Select 01.mkv", "Select 03.mkv", "Select 06.mkv"]);

    await user.click(screen.getByRole("button", { name: "Use 04 as template" }));
    expect(checkedNames()).toEqual(["Select 04.mkv", "Select 05.mkv", "Select 06.mkv"]);
    expect(screen.getByText("3 files differ from the template and are not selected.")).toBeInTheDocument();
  });

  it("does not bring old manual choices back when the template changes back", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(box("06.mkv"));
    await user.click(screen.getByRole("button", { name: "Use 04 as template" }));
    await user.click(screen.getByRole("button", { name: "Use 01 as template" }));
    expect(checkedNames()).toEqual(["Select 01.mkv", "Select 02.mkv", "Select 03.mkv"]);
  });

  it("keeps a manual selection when a rescan leaves the matching files unchanged", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.click(box("02.mkv"));
    await user.click(screen.getByRole("button", { name: "Refresh files" }));
    await user.click(screen.getByRole("button", { name: "Add a differing file" }));
    expect(checkedNames()).toEqual(["Select 01.mkv", "Select 03.mkv"]);
    expect(screen.getByText("4 files differ from the template and are not selected.")).toBeInTheDocument();
  });

  it("selects every file when none differs and shows no notice", () => {
    render(<Harness initialFiles={files.slice(0, 3)} />);

    expect(checkedNames()).toHaveLength(3);
    expect(screen.queryByText(/from the template/)).not.toBeInTheDocument();
  });

  it("selects nothing while there is no template", async () => {
    const user = userEvent.setup();
    render(<Harness initialTemplatePath="" />);

    expect(checkedNames()).toEqual([]);
    expect(screen.getByText("0 of 6 selected")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Only equal to template" }));
    expect(checkedNames()).toEqual([]);

    await user.click(screen.getByRole("button", { name: "Use 04 as template" }));
    expect(checkedNames()).toEqual(["Select 04.mkv", "Select 05.mkv", "Select 06.mkv"]);
  });
});

describe("FileSelectionTable filter", () => {
  it("narrows the rows by name and scopes the toolbar to what is shown", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.type(screen.getByRole("searchbox", { name: "Filter files by name" }), "0");
    expect(screen.queryByText(/shown/)).toHaveTextContent("6 shown");
    await user.clear(screen.getByRole("searchbox", { name: "Filter files by name" }));
    await user.type(screen.getByRole("searchbox", { name: "Filter files by name" }), "02");
    expect(screen.getAllByRole("checkbox")).toHaveLength(1);
    expect(screen.getByText(/shown/)).toHaveTextContent("1 shown");

    // A hidden selected file stays selected, and the count says so.
    await user.click(screen.getByRole("button", { name: "Select none" }));
    expect(screen.getByRole("status")).toHaveTextContent("/media/01.mkv,/media/03.mkv");
    expect(screen.getByText(/of 6 selected/)).toHaveTextContent("2 of 6 selected");
    expect(screen.getByText("2 selected files are hidden by the filter and will be included.")).toBeInTheDocument();
    expect(screen.getByLabelText("hidden")).toHaveTextContent("2");

    await user.clear(screen.getByRole("searchbox", { name: "Filter files by name" }));
    await user.type(screen.getByRole("searchbox", { name: "Filter files by name" }), "05");
    await user.click(screen.getByRole("button", { name: "Select all" }));
    expect(screen.getByRole("status")).toHaveTextContent("/media/01.mkv,/media/03.mkv,/media/05.mkv");
  });

  it("says when no name matches", async () => {
    const user = userEvent.setup();
    render(<Harness />);

    await user.type(screen.getByRole("searchbox", { name: "Filter files by name" }), "Movie");
    expect(screen.getByText("No file names contain “Movie”.")).toBeInTheDocument();
    expect(screen.queryAllByRole("checkbox")).toHaveLength(0);
  });
});

function ActiveHarness({ columns }: { columns?: FileColumn[] }) {
  const selection = useFileSelection(files, files[0]);
  const [active, setActive] = useState(files[0].path);
  return (
    <>
      <output aria-label="active">{active}</output>
      <FileSelectionTable title="File Info" files={files} templateFile={files[0]} selection={selection} columns={columns} label="Operation files" activePath={active} onActivate={setActive} />
    </>
  );
}

function row(name: string) {
  return box(name).closest("tr")!;
}

describe("FileSelectionTable rows that pick a file", () => {
  it("picks a row by click without changing its checkbox", async () => {
    const user = userEvent.setup();
    render(<ActiveHarness />);

    expect(row("01.mkv")).toHaveAttribute("aria-current", "true");
    await user.click(screen.getByText("04.mkv"));
    expect(screen.getByRole("status", { name: "active" })).toHaveTextContent("/media/04.mkv");
    expect(row("04.mkv")).toHaveAttribute("aria-current", "true");
    expect(box("04.mkv")).not.toBeChecked();

    await user.click(box("05.mkv"));
    expect(box("05.mkv")).toBeChecked();
    expect(screen.getByRole("status", { name: "active" })).toHaveTextContent("/media/04.mkv");
  });

  it("is operated from the keyboard: arrows move, Space checks, Enter picks", async () => {
    const user = userEvent.setup();
    render(<ActiveHarness />);

    // Only the picked row is in the tab order.
    expect(row("01.mkv")).toHaveAttribute("tabindex", "0");
    expect(row("02.mkv")).toHaveAttribute("tabindex", "-1");
    row("01.mkv").focus();
    await user.keyboard("{ArrowDown}{ArrowDown}{ArrowDown}");
    expect(row("04.mkv")).toHaveFocus();
    expect(screen.getByRole("status", { name: "active" })).toHaveTextContent("/media/04.mkv");

    await user.keyboard(" ");
    expect(box("04.mkv")).toBeChecked();
    await user.keyboard("{ArrowDown}{Shift>} {/Shift}");
    expect(box("05.mkv")).toBeChecked();

    await user.keyboard("{End}");
    expect(row("06.mkv")).toHaveFocus();
    await user.keyboard("{Home}{Enter}");
    expect(screen.getByRole("status", { name: "active" })).toHaveTextContent("/media/01.mkv");
    expect(screen.getByLabelText("Operation files")).toContainElement(row("01.mkv"));
  });

  it("shows the columns a page asks for, sortable", async () => {
    const user = userEvent.setup();
    const tracks: FileColumn = { key: "tracks", label: "Tracks", sortValue: (item) => String(item.file.tracks.length), cell: (item) => <em>{item.file.tracks.length} tracks</em> };
    render(<ActiveHarness columns={[fileColumns.reader, fileColumns.resolution, tracks]} />);

    expect(screen.getByRole("button", { name: "Sort by Reader" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Sort by Status" })).not.toBeInTheDocument();
    expect(within(row("01.mkv")).getByText("2 tracks")).toBeInTheDocument();
    expect(within(row("01.mkv")).getByText("1920x1080")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Sort by Tracks" }));
    expect(screen.getByRole("button", { name: "Sort by Tracks" }).closest("th")).toHaveAttribute("aria-sort", "ascending");
  });
});

function LibraryHarness() {
  const selection = useLibrarySelection(files.slice(0, 3), files[0]);
  return <FileSelectionTable title="File Info" files={files.slice(0, 3)} templateFile={files[0]} selection={selection} matchingAction={false} />;
}

describe("useLibrarySelection", () => {
  it("reads and writes the shared selection, leaving files outside the table alone", async () => {
    const user = userEvent.setup();
    // The backend may hand paths back with other separators or case.
    localStorage.setItem("mkvo.web.selectedPaths", JSON.stringify(["\\MEDIA\\02.mkv", "/media/Movie.mp4"]));
    const setFileSelection = vi.fn((paths: string[]) => Promise.resolve({ updatedUtc: null, files: [], selectedPaths: paths, summary: { total: 0, mkv: 0, mp4: 0, failed: 0, cached: 0 } }));
    renderWithBackend(<MediaLibraryProvider><LibraryHarness /></MediaLibraryProvider>, { setFileSelection });

    expect(checkedNames()).toEqual(["Select 02.mkv"]);
    expect(screen.queryByRole("button", { name: "Only equal to template" })).not.toBeInTheDocument();
    await user.click(box("03.mkv"));
    await waitFor(() => expect(setFileSelection).toHaveBeenLastCalledWith(["/media/Movie.mp4", "/media/02.mkv", "/media/03.mkv"]));
    await user.click(screen.getByRole("button", { name: "Select none" }));
    await waitFor(() => expect(setFileSelection).toHaveBeenLastCalledWith(["/media/Movie.mp4"]));
    localStorage.clear();
  });
});

describe("FileSelectionTable colours", () => {
  it("tints whole rows only when asked, and otherwise colours just the status", () => {
    const { unmount } = render(<Harness />);
    expect(row("04.mkv")).toHaveClass("text-warning");
    expect(row("01.mkv")).toHaveClass("text-template");
    unmount();

    render(<ActiveHarness />);
    expect(row("04.mkv")).not.toHaveClass("text-warning");
    expect(row("01.mkv")).not.toHaveClass("text-template");
    expect(within(row("04.mkv")).getByRole("button", { name: "Warning (1)" }).closest("td")).toHaveClass("text-warning");
    expect(within(row("01.mkv")).getByText("Template")).toHaveClass("text-template");
    expect(within(row("02.mkv")).getByText("Scanned")).toHaveClass("text-success");
  });
});

describe("FileSelectionTable accessibility", () => {
  it("keeps picking rows in one tab stop and exposes their checked state on the row", async () => {
    const user = userEvent.setup();
    render(<ActiveHarness />);

    expect(screen.getByRole("grid")).toHaveAttribute("aria-multiselectable", "true");
    expect(box("01.mkv")).toHaveAttribute("tabindex", "-1");
    expect(row("01.mkv")).toHaveAttribute("aria-selected", "true");
    expect(row("04.mkv")).toHaveAttribute("aria-selected", "false");
    row("04.mkv").focus();
    await user.keyboard(" ");
    expect(row("04.mkv")).toHaveAttribute("aria-selected", "true");
  });

  it("announces the selected count politely", () => {
    render(<Harness />);
    expect(screen.getByText("3 of 6 selected").closest("[aria-live]")).toHaveAttribute("aria-live", "polite");
  });
});
