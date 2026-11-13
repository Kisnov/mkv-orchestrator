import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";

import { useAutosave, type AutosaveOptions } from "./useAutosave";

type Request = { value: string };

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

function setup(save: (request: Request) => Promise<string>, initialProps = { value: "loaded", ready: true }) {
  const onSaved = vi.fn();
  const render = renderHook(
    ({ value, ready }: { value: string; ready: boolean }) => useAutosave<Request, string>({
      request: { value },
      ready,
      delayMillis: 100,
      save,
      onPending: () => undefined,
      onSaving: () => undefined,
      onSaved,
      onError: () => undefined
    } satisfies AutosaveOptions<Request, string>),
    { initialProps }
  );
  return { ...render, onSaved };
}

describe("useAutosave", () => {
  beforeEach(() => { vi.useFakeTimers(); });
  afterEach(() => { vi.useRealTimers(); });

  it("does not save the values it was loaded with", async () => {
    const save = vi.fn().mockResolvedValue("saved");
    setup(save);

    await act(() => vi.advanceTimersByTimeAsync(500));

    expect(save).not.toHaveBeenCalled();
  });

  it("waits for the edits to settle before saving", async () => {
    const save = vi.fn().mockResolvedValue("saved");
    const { rerender } = setup(save);

    rerender({ value: "a", ready: true });
    await act(() => vi.advanceTimersByTimeAsync(50));
    rerender({ value: "ab", ready: true });
    await act(() => vi.advanceTimersByTimeAsync(100));

    expect(save).toHaveBeenCalledTimes(1);
    expect(save).toHaveBeenCalledWith({ value: "ab" });
  });

  /// Two saves in flight could answer in either order, and the older answer
  /// would win. One at a time, the second sends the newest edit.
  it("runs one save at a time and the next one sends the latest edit", async () => {
    const first = deferred<string>();
    const save = vi.fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue("second");
    const { rerender, onSaved } = setup(save);

    rerender({ value: "one", ready: true });
    await act(() => vi.advanceTimersByTimeAsync(100));
    rerender({ value: "two", ready: true });
    await act(() => vi.advanceTimersByTimeAsync(100));
    rerender({ value: "three", ready: true });
    await act(() => vi.advanceTimersByTimeAsync(100));

    expect(save).toHaveBeenCalledTimes(1);
    await act(async () => { first.resolve("first"); });

    expect(save.mock.calls.map(([request]) => request.value)).toEqual(["one", "three"]);
    expect(onSaved.mock.calls.at(-1)).toEqual(["second", true]);
    expect(onSaved.mock.calls.filter(([, latest]) => latest)).toHaveLength(1);
  });

  it("saves an edit back to the old value made while a save was in flight", async () => {
    const first = deferred<string>();
    const save = vi.fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue("second");
    const { rerender } = setup(save);

    rerender({ value: "one", ready: true });
    await act(() => vi.advanceTimersByTimeAsync(100));
    rerender({ value: "loaded", ready: true });
    await act(async () => { first.resolve("first"); });

    expect(save.mock.calls.map(([request]) => request.value)).toEqual(["one", "loaded"]);
  });

  it("flushes on demand behind the save in flight and returns its response", async () => {
    const first = deferred<string>();
    const save = vi.fn()
      .mockImplementationOnce(() => first.promise)
      .mockResolvedValue("flushed");
    const { result, rerender } = setup(save);

    rerender({ value: "one", ready: true });
    await act(() => vi.advanceTimersByTimeAsync(100));
    rerender({ value: "two", ready: true });
    let flushed: Promise<string | null> | undefined;
    act(() => { flushed = result.current(); });
    expect(save).toHaveBeenCalledTimes(1);

    await act(async () => { first.resolve("first"); });

    await expect(flushed).resolves.toBe("flushed");
    expect(save).toHaveBeenLastCalledWith({ value: "two" });
  });

  it("takes its baseline only once the form is ready", async () => {
    const save = vi.fn().mockResolvedValue("saved");
    const { rerender } = setup(save, { value: "defaults", ready: false });
    rerender({ value: "hydrated", ready: true });
    await act(() => vi.advanceTimersByTimeAsync(500));
    expect(save).not.toHaveBeenCalled();

    rerender({ value: "edited", ready: true });
    await act(() => vi.advanceTimersByTimeAsync(100));
    expect(save).toHaveBeenCalledWith({ value: "edited" });
  });
});
