import { describe, expect, it } from "vitest";
import { PATIENCE_WAITS, waitsFor } from "#/modules/volume-run/patience";

describe("PATIENCE_WAITS", () => {
  it("starts at five seconds and doubles to a five-minute cap", () => {
    expect(PATIENCE_WAITS.slice(0, 8)).toEqual([5, 10, 20, 40, 80, 160, 300, 300]);
    expect(Math.max(...PATIENCE_WAITS)).toBe(300);
  });

  it("adds up to exactly six hours in 77 checks", () => {
    expect(PATIENCE_WAITS.reduce((sum, wait) => sum + wait, 0)).toBe(6 * 60 * 60);
    expect(PATIENCE_WAITS).toHaveLength(77);
    expect(PATIENCE_WAITS.at(-1)).toBe(285);
  });
});

describe("waitsFor", () => {
  it("backs off the same way and clips the last wait to the bound", () => {
    expect(waitsFor(15 * 60)).toEqual([5, 10, 20, 40, 80, 160, 300, 285]);
  });
});
