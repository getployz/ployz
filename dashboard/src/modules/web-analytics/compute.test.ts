import { describe, expect, it } from "vitest";
import { breakdown, entryPages, goalConversions, overview, rates, sessions, timeseries } from "./compute";

const value = (value: string, pageviews: number, visitors: number) => ({ value, pageviews, visitors });
const session = (value: string, visits: number, bounces = 0, totalDuration = 0) => ({ value, visits, bounces, totalDuration });

describe("ordering", () => {
  it("breakdown orders by visitors, then by value so a tie is stable", () => {
    const rows = [value("/pricing", 9, 3), value("/about", 1, 3), value("/", 50, 4), value("/zebra", 2, 1)];
    expect(breakdown(rows, 10).map((row) => row.value)).toEqual(["/", "/about", "/pricing", "/zebra"]);
  });

  it("sessions order by visits, entry pages by visitors, goals by visitors then name", () => {
    expect(sessions([session("google", 2), session("direct", 5), session("bing", 2)], 10).map((row) => row.value))
      .toEqual(["direct", "bing", "google"]);
    const entries = [{ ...session("/b", 9), visitors: 1 }, { ...session("/a", 1), visitors: 7 }, { ...session("/c", 1), visitors: 7 }];
    expect(entryPages(entries, 10).map((row) => row.value)).toEqual(["/a", "/c", "/b"]);
    const goals = [
      { goalId: "2", name: "Signup", visitors: 3, conversions: 9 },
      { goalId: "1", name: "Checkout", visitors: 3, conversions: 1 },
      { goalId: "3", name: "Download", visitors: 8, conversions: 8 },
    ];
    expect(goalConversions(goals).map((row) => row.name)).toEqual(["Download", "Checkout", "Signup"]);
  });
});

describe("limit", () => {
  it("merges a value split across rollup and raw parts before cutting to the limit", () => {
    const rows = [value("/split", 1, 2), value("/big", 10, 5), value("/mid", 4, 4), value("/split", 1, 3)];
    expect(breakdown(rows, 2)).toEqual([value("/big", 10, 5), value("/split", 2, 5)]);
  });

  it("sums every session field of a split value", () => {
    const rows = [session("google", 2, 1, 30), session("direct", 1, 0, 5), session("google", 3, 2, 70)];
    expect(sessions(rows, 1)).toEqual([{ value: "google", visits: 5, bounces: 3, totalDuration: 100, bounceRatePercent: 60, avgDurationSeconds: 20 }]);
  });
});

describe("rounding", () => {
  it("bounce rate is a percentage to one decimal and average duration a whole second", () => {
    expect(rates({ visits: 3, bounces: 1, totalDuration: 100 })).toEqual({ bounceRatePercent: 33.3, avgDurationSeconds: 33 });
    expect(rates({ visits: 3, bounces: 2, totalDuration: 101 })).toEqual({ bounceRatePercent: 66.7, avgDurationSeconds: 34 });
    expect(rates({ visits: 8, bounces: 1, totalDuration: 7 })).toEqual({ bounceRatePercent: 12.5, avgDurationSeconds: 1 });
  });

  it("no visits means zero rates, not NaN", () => {
    expect(rates({ visits: 0, bounces: 0, totalDuration: 0 })).toEqual({ bounceRatePercent: 0, avgDurationSeconds: 0 });
    expect(overview([])).toEqual({ pageviews: 0, visitors: 0, visits: 0, bounces: 0, totalDuration: 0, bounceRatePercent: 0, avgDurationSeconds: 0 });
  });
});

describe("timeseries", () => {
  const totals = (bucket: Date, pageviews: number) => ({ bucket, pageviews, visitors: 1, visits: 1, bounces: 0, totalDuration: 0 });

  it("fills every local day with zeros and gives the Sydney DST day its 23 hours", () => {
    const start = new Date("2026-10-02T14:00:00Z");
    const end = new Date("2026-10-05T13:00:00Z");
    const days = timeseries([totals(new Date("2026-10-03T14:00:00Z"), 4)], { start, end }, "Australia/Sydney", "day");
    expect(days.map((day) => [day.bucket.toISOString(), day.pageviews])).toEqual([
      ["2026-10-02T14:00:00.000Z", 0],
      ["2026-10-03T14:00:00.000Z", 4],
      ["2026-10-04T13:00:00.000Z", 0],
    ]);
    const hours = timeseries([], { start: new Date("2026-10-03T14:00:00Z"), end }, "Australia/Sydney", "hour");
    expect(hours).toHaveLength(23 + 24);
  });

  it("sums rows that land in one bucket and ignores rows outside the range", () => {
    const start = new Date("2026-10-01T00:00:00Z");
    const [first, second] = timeseries(
      [totals(start, 1), totals(start, 2), totals(new Date("2026-10-03T00:00:00Z"), 9)],
      { start, end: new Date("2026-10-01T02:00:00Z") },
      "UTC",
      "hour",
    );
    expect(first).toMatchObject({ pageviews: 3, visitors: 2, visits: 2 });
    expect(second).toMatchObject({ pageviews: 0, visits: 0, bounceRatePercent: 0 });
  });
});
