import { useCallback, useEffect, useRef } from "react";

export type AutosaveOptions<Request, Response> = {
  /** What the form would save right now. */
  request: Request;
  /** False until the form holds the loaded values; the first ready request is the baseline. */
  ready: boolean;
  delayMillis: number;
  save: (request: Request) => Promise<Response>;
  onPending: () => void;
  onSaving: () => void;
  /** `latest` is false when another save is already queued behind this one. */
  onSaved: (response: Response, latest: boolean) => void;
  onError: (error: unknown) => void;
};

/**
 * Saves the request a moment after it stops changing. Saves run one at a time
 * and each sends the form as it is when it starts, so a slow response can
 * neither be overtaken by an older one nor stand in for newer edits.
 * Returns `flush`, which saves now (after any save in flight).
 */
export function useAutosave<Request, Response>(options: AutosaveOptions<Request, Response>) {
  const { request, ready, delayMillis } = options;
  const fingerprint = JSON.stringify(request);
  const latest = useRef({ request, fingerprint, options });
  const savedFingerprint = useRef<string | null>(null);
  const queue = useRef<Promise<unknown>>(Promise.resolve());
  // A save waiting for the one in flight; later flushes join it, as it reads the form when it starts.
  const waiting = useRef<Promise<Response | null> | null>(null);
  const timer = useRef<number | null>(null);

  useEffect(() => {
    latest.current = { request, fingerprint, options };
  });

  const flush = useCallback((): Promise<Response | null> => {
    function enqueue(): Promise<Response | null> {
      if (waiting.current) return waiting.current;
      const run = async () => {
        waiting.current = null;
        const { request: sent, fingerprint: sentFingerprint, options: current } = latest.current;
        current.onSaving();
        try {
          const response = await current.save(sent);
          savedFingerprint.current = sentFingerprint;
          // An edit back to the old value schedules nothing, yet differs from what was just saved.
          if (!waiting.current && !timer.current && latest.current.fingerprint !== sentFingerprint) void enqueue();
          latest.current.options.onSaved(response, waiting.current === null);
          return response;
        } catch (error) {
          latest.current.options.onError(error);
          return null;
        }
      };
      const next = queue.current.then(run);
      waiting.current = next;
      queue.current = next;
      return next;
    }
    return enqueue();
  }, []);

  useEffect(() => {
    if (!ready) return;
    if (savedFingerprint.current === null) {
      savedFingerprint.current = fingerprint;
      return;
    }
    if (fingerprint === savedFingerprint.current) return;
    latest.current.options.onPending();
    const scheduled = window.setTimeout(() => {
      timer.current = null;
      // A flush on demand may already have saved this.
      if (latest.current.fingerprint !== savedFingerprint.current) void flush();
    }, delayMillis);
    timer.current = scheduled;
    return () => {
      window.clearTimeout(scheduled);
      timer.current = null;
    };
  }, [delayMillis, fingerprint, flush, ready]);

  return flush;
}
