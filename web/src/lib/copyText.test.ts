import { afterEach, expect, it, vi } from "vitest";

import { copyText } from "./copyText";

afterEach(() => vi.unstubAllGlobals());

it("uses the clipboard API where the page has it", async () => {
  const writeText = vi.fn(() => Promise.resolve());
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });

  expect(await copyText("log")).toBe(true);
  expect(writeText).toHaveBeenCalledWith("log");
});

it("falls back to a hidden text area on a plain HTTP origin, where the clipboard API is missing", async () => {
  vi.stubGlobal("navigator", { ...navigator, clipboard: undefined });
  let copied = "";
  const execCommand = vi.fn(() => { copied = (document.activeElement as HTMLTextAreaElement).value; return true; });
  Object.defineProperty(document, "execCommand", { value: execCommand, configurable: true });

  expect(await copyText("line 1\nline 2")).toBe(true);
  expect(execCommand).toHaveBeenCalledWith("copy");
  expect(copied).toBe("line 1\nline 2");
  expect(document.querySelector("textarea")).toBeNull();
});

it("says so when nothing could copy", async () => {
  vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText: () => Promise.reject(new Error("denied")) } });
  Object.defineProperty(document, "execCommand", { value: () => false, configurable: true });

  expect(await copyText("log")).toBe(false);
});
