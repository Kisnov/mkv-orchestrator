import { useState } from "react";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import type { MediaFileRow } from "../api";
import { useFileSelection } from "../state/useFileSelection";
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

function Harness({ initialFiles = files }: { initialFiles?: MediaFileRow[] }) {
  const [templatePath, setTemplatePath] = useState(initialFiles[0].path);
  const [scanned, setScanned] = useState(initialFiles);
  const templateFile = scanned.find((item) => item.path === templatePath) ?? null;
  const selection = useFileSelection(scanned, templateFile);
  return (
    <>
      <button type="button" onClick={() => setTemplatePath("/media/04.mkv")}>Use 04 as template</button>
      <button type="button" onClick={() => setScanned((current) => current.map((item) => ({ ...item })))}>Refresh files</button>
      <button type="button" onClick={() => setScanned((current) => [...current, file("07.mkv", "jpn")])}>Add a differing file</button>
      <output>{selection.selectedPaths.join(",")}</output>
      <FileSelectionTable title="Files" files={scanned} templateFile={templateFile} selection={selection} />
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
});
