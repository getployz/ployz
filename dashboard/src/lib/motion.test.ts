import { afterEach, expect, it, vi } from "vitest";
import { prefersReducedMotion } from "./motion";

afterEach(() => vi.unstubAllGlobals());

it("uses the current preference on every action, including after it changes", () => {
  let matches = false;
  vi.stubGlobal("matchMedia", vi.fn(() => ({ matches })));
  expect(prefersReducedMotion()).toBe(false);
  matches = true;
  expect(prefersReducedMotion()).toBe(true);
});

it("is safe when rendering on the server", () => {
  vi.stubGlobal("matchMedia", undefined);
  expect(prefersReducedMotion()).toBe(false);
});
