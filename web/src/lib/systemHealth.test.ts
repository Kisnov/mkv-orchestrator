import { expect, it } from "vitest";

import { shortToolVersion, watchState } from "./systemHealth";

const now = new Date("2026-11-14T12:00:00Z").getTime();

it("reads the version number out of a tool banner", () => {
  expect(shortToolVersion("mkvmerge v102.0 ('Little Houses') 64-bit")).toBe("102.0");
  expect(shortToolVersion("ffmpeg version 7.1.1-essentials_build-www.gyan.dev Copyright (c) 2000-2025\nbuilt with gcc")).toBe("7.1.1");
  expect(shortToolVersion("ffprobe version 2026-07-30-git-2ae2413488-full_build")).toBe("2026-07-30");
  expect(shortToolVersion("custom build")).toBe("custom build");
});

it("describes the watcher by what it reports before what the setting says", () => {
  expect(watchState(false, { running: false, backend: "native", watchedRoots: 0 }, false, now)).toMatchObject({ health: "off", name: "Off" });
  expect(watchState(true, { running: true, backend: "native", watchedRoots: 1 }, false, now)).toEqual({ health: "ok", name: "Watching 1 folder", text: "File system events, no changes seen yet" });
  expect(watchState(true, { running: true, backend: "polling", watchedRoots: 3, lastEventUtc: "2026-11-14T11:55:00Z" }, false, now)).toEqual({ health: "ok", name: "Watching 3 folders", text: "Polling, last change 5 minutes ago" });
  expect(watchState(true, { running: false, backend: "native", watchedRoots: 0, error: "The folder is gone." }, false, now)).toEqual({ health: "problem", name: "Not running", text: "The folder is gone." });
  expect(watchState(true, { running: false, backend: "native", watchedRoots: 0 }, false, now)).toMatchObject({ health: "problem", name: "Not running" });
  // Running although the saved setting says off: the watcher is believed.
  expect(watchState(false, { running: true, backend: "native", watchedRoots: 2 }, false, now)).toMatchObject({ health: "ok", name: "Watching 2 folders" });
  expect(watchState(true, undefined, true, now)).toMatchObject({ health: "problem", name: "Unknown" });
  expect(watchState(true, undefined, false, now)).toMatchObject({ health: "checking" });
});
