import type { WatchHealth } from "../api";
import { timeAgo } from "./jobActivity";

export type Health = "ok" | "problem" | "off" | "checking";
export type WatchState = { health: Health; name: string; text: string };

/** The folder watcher in two lines. What the watcher reports wins over the setting. */
export function watchState(enabled: boolean, health: WatchHealth | undefined, failed: boolean, now: number): WatchState {
  if (health?.running) {
    const change = health.lastEventUtc ? `last change ${timeAgo(health.lastEventUtc, now)}` : "no changes seen yet";
    return { health: health.error ? "problem" : "ok", name: `Watching ${health.watchedRoots} ${health.watchedRoots === 1 ? "folder" : "folders"}`, text: health.error ?? `${health.backend === "polling" ? "Polling" : "File system events"}, ${change}` };
  }
  if (!enabled) return { health: "off", name: "Off", text: "Folders are not watched for changes." };
  if (health) return { health: "problem", name: "Not running", text: health.error ?? "Watching is turned on, but the watcher is not running." };
  return failed
    ? { health: "problem", name: "Unknown", text: "The watcher's state could not be read." }
    : { health: "checking", name: "Checking", text: "Reading the watcher's state." };
}

/** The version number out of a tool's banner: "mkvmerge v102.0 ('Little Houses') 64-bit" is "102.0". */
export function shortToolVersion(banner: string) {
  const line = banner.split(/\r?\n/)[0].trim();
  return /\d+(?:[.-]\d+)+/.exec(line)?.[0] ?? line;
}
