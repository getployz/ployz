import { expect, it } from "vitest";
import { PATIENCE_WAITS, waitsFor } from "#/modules/volume-run/patience";

const sum = (waits: ReadonlyArray<number>) => waits.reduce((total, wait) => total + wait, 0);

it("waits exactly to the bound, never past it", () => {
  expect(sum(PATIENCE_WAITS)).toBe(6 * 60 * 60);
  for (const bound of [5, 60, 15 * 60, 6 * 60 * 60]) {
    expect(sum(waitsFor(bound)), `${bound}s`).toBe(bound);
  }
});
