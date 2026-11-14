const storageKey = "mkvo.web.jobLabels";
const remembered = 60;

/** Job id to the name of the page that started it, for the jobs this browser started. */
export function readJobLabels(): Map<string, string> {
  try {
    return new Map(JSON.parse(localStorage.getItem(storageKey) ?? "[]") as Array<[string, string]>);
  } catch {
    return new Map();
  }
}

/** Kept beyond the job itself, so the Dashboard can name and reopen it later. */
export function rememberJobLabel(id: string, label: string) {
  try {
    const labels = [...readJobLabels()].filter(([known]) => known !== id);
    localStorage.setItem(storageKey, JSON.stringify([...labels, [id, label]].slice(-remembered)));
  } catch { /* the job is only named by its kind */ }
}

const dismissedKey = "mkvo.web.dismissedJobFailures";

/** The failed jobs the user has acknowledged on the Dashboard. */
export function readDismissedFailures(): string[] {
  try {
    return JSON.parse(localStorage.getItem(dismissedKey) ?? "[]") as string[];
  } catch {
    return [];
  }
}

export function rememberDismissedFailure(id: string) {
  try {
    localStorage.setItem(dismissedKey, JSON.stringify([...readDismissedFailures().filter((known) => known !== id), id].slice(-remembered)));
  } catch { /* dismissed for this visit only */ }
}
