import { describe, expect, it } from "vitest";
import { bytesIn, bytesText, cpuIn, cpuText, onGrid, rateIn, rateText } from "./metric-chart-data";

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

  it("says every tick on an axis in the unit of its top", () => {
    expect([0, 500e6, 1e9].map(bytesIn(1.15 * 1024 ** 3))).toEqual(["0 GB", "0.5 GB", "0.9 GB"]);
    expect([0, 512 * 1024, 2 * 1024 ** 2].map(rateIn(2.3 * 1024 ** 2))).toEqual(["0 MB/s", "0.5 MB/s", "2 MB/s"]);
  });

  it("says CPU as a share of one core", () => {
    expect([0, 0.0003, 0.012, 0.125, 0.5, 1.999].map(cpuText)).toEqual(["0%", "0.03%", "1.2%", "13%", "50%", "200%"]);
  });

  it("says every CPU tick with the decimals its axis needs", () => {
    expect([0, 0.002, 0.004].map(cpuIn(0.0046))).toEqual(["0%", "0.2%", "0.4%"]);
    expect([0, 0.0002, 0.0004].map(cpuIn(0.00046))).toEqual(["0%", "0.02%", "0.04%"]);
    expect([0, 0.5, 1].map(cpuIn(1.15))).toEqual(["0%", "50%", "100%"]);
  });
});
