/** Compares paths the way the backend does: separator- and case-insensitive. */
export function pathKey(path: string) {
  return path.replace(/\\/g, "/").toLowerCase();
}
