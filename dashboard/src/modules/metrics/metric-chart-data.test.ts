import { describe, expect, it } from "vitest";
import { bytesText, cpuText, onGrid, rateText } from "./metric-chart-data";

const MINUTE = 60_000;

describe("onGrid", () => {
  const window = { from: 0, to: 4 * MINUTE, stepMs: MINUTE };

  it("averages the samples in each step and leaves a step without one null", () => {
    const samples = [
      { at: 0, value: 2 },
      { at: 30_000, value: 4 },
      { at: 2 * MINUTE, value: 0 },
      { at: 3 * MINUTE + 10_000, value: 7 },
    ];
    expect(onGrid(samples, window)).toEqual([3, null, 0, 7]);
  });

  it("drops samples outside the window and counts one at its end in the last step", () => {
    expect(onGrid([{ at: -1, value: 9 }, { at: 4 * MINUTE, value: 5 }, { at: 5 * MINUTE, value: 9 }], window))
      .toEqual([null, null, null, 5]);
  });
});

describe("formatters", () => {
  it("says bytes in the largest unit with one decimal under 10", () => {
    expect([0, 812, 1536, 318 * 1024 * 1024, 1.4 * 1024 ** 3, 2 * 1024 ** 3].map(bytesText))
      .toEqual(["0 B", "812 B", "1.5 KB", "318 MB", "1.4 GB", "2 GB"]);
    expect(rateText(885 * 1024)).toBe("885 KB/s");
  });

  it("says vCPUs to two decimals", () => {
    expect([0, 0.125, 0.5, 1.999].map(cpuText)).toEqual(["0", "0.13", "0.5", "2"]);
  });
});
