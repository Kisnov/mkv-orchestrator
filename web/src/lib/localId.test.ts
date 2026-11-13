import { afterEach, expect, it, vi } from "vitest";

import { createLocalId } from "./localId";

const uuid = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

afterEach(() => vi.unstubAllGlobals());

it("uses randomUUID where the page has it", () => {
  vi.stubGlobal("crypto", { randomUUID: () => "11111111-2222-4333-8444-555555555555", getRandomValues: crypto.getRandomValues.bind(crypto) });

  expect(createLocalId()).toBe("11111111-2222-4333-8444-555555555555");
});

it("builds a version 4 UUID on a plain HTTP origin, where randomUUID is missing", () => {
  vi.stubGlobal("crypto", { getRandomValues: crypto.getRandomValues.bind(crypto) });

  const first = createLocalId();
  expect(first).toMatch(uuid);
  expect(createLocalId()).not.toBe(first);
});
