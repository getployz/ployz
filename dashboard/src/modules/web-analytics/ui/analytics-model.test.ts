import { describe, expect, it } from "vitest";
import { isHourStart } from "../local-time";
import { analyticsGate, customRange, goalRate, isRangeSpec, parsePageFilter, partialDays, resolveRange, viewsPerVisit } from "./analytics-model";

const iso = (range: { start: Date; end: Date }) => [range.start.toISOString(), range.end.toISOString()];
const NOW = Date.parse("2026-10-15T09:20:00Z");

describe("resolveRange", () => {
  it("reads 30 site-local days through the end of the current hour, by day", () => {
    const { range, interval } = resolveRange("30d", "UTC", NOW);
    expect(iso(range)).toEqual(["2026-09-16T00:00:00.000Z", "2026-10-15T10:00:00.000Z"]);
    expect(interval).toBe("day");
  });

  it("reads today by hour from the site's midnight", () => {
    const { range, interval } = resolveRange("today", "Australia/Sydney", NOW);
    expect(range.start.toISOString()).toBe("2026-10-14T13:00:00.000Z");
    expect(interval).toBe("hour");
  });

  it("covers a custom range's last day whole, and stops a range reaching today at the current hour", () => {
    expect(iso(resolveRange(customRange("2026-10-01", "2026-10-03"), "UTC", NOW).range))
      .toEqual(["2026-10-01T00:00:00.000Z", "2026-10-04T00:00:00.000Z"]);
    expect(iso(resolveRange(customRange("2026-10-14", "2026-10-20"), "UTC", NOW).range))
      .toEqual(["2026-10-14T00:00:00.000Z", "2026-10-15T10:00:00.000Z"]);
    expect(resolveRange(customRange("2026-10-02", "2026-10-02"), "UTC", NOW).interval).toBe("hour");
  });

  it("ends at the next local hour boundary when a half-hour offset change shortens the hour", () => {
    const zone = "Australia/Lord_Howe";
    // Spring forward: local 02:00 jumps to 02:30, so the hour holding 02:45 ends at 03:00.
    const spring = resolveRange("today", zone, Date.parse("2026-10-03T15:45:00Z"));
    expect(iso(spring.range)).toEqual(["2026-10-03T13:30:00.000Z", "2026-10-03T16:00:00.000Z"]);
    // Fall back: the repeated 01:30 to 02:00 is its own half-hour bucket.
    const fall = resolveRange("today", zone, Date.parse("2026-04-04T15:15:00Z"));
    expect(iso(fall.range)).toEqual(["2026-04-04T13:00:00.000Z", "2026-04-04T15:30:00.000Z"]);
    for (const now of ["2026-10-03T15:45:00Z", "2026-04-04T15:15:00Z"]) {
      for (const spec of ["today", "7d", "30d", customRange("2026-09-20", "2026-10-20")]) {
        const { range } = resolveRange(spec, zone, Date.parse(now));
        expect([isHourStart(zone, range.start), isHourStart(zone, range.end)]).toEqual([true, true]);
      }
    }
  });

  it("reads the default range for a custom range that starts after now", () => {
    expect(resolveRange(customRange("2030-01-01", "2030-01-02"), "UTC", NOW)).toEqual(resolveRange("30d", "UTC", NOW));
  });

  it("refuses a spec it cannot read", () => {
    expect(isRangeSpec("7d")).toBe(true);
    expect(isRangeSpec(customRange("2026-10-02", "2026-10-01"))).toBe(false);
    expect(isRangeSpec(customRange("2026-02-30", "2026-03-01"))).toBe(false);
    expect(isRangeSpec("2026-10-01")).toBe(false);
    expect(isRangeSpec("90d")).toBe(false);
  });
});

describe("partialDays", () => {
  it("marks days with uncounted visits, then today", () => {
    const days = ["2026-10-12", "2026-10-13", "2026-10-14", "2026-10-15"];
    const gaps = [{ day: "2026-10-13", count: 4 }, { day: "2026-10-14", count: 0 }];
    expect(partialDays(days, gaps, "2026-10-15")).toEqual([
      { day: "2026-10-13", reason: "gap" },
      { day: "2026-10-15", reason: "today" },
    ]);
  });

  it("calls today a gap when visits were lost on it", () => {
    expect(partialDays(["2026-10-15"], [{ day: "2026-10-15", count: 1 }], "2026-10-15")).toEqual([{ day: "2026-10-15", reason: "gap" }]);
  });
});

describe("goalRate", () => {
  it("is the share of the range's visitors who reached the goal, not views per goal visitor", () => {
    expect(goalRate(84_862, 240_090)).toBeCloseTo(35.35, 2);
    expect(goalRate(0, 240_090)).toBe(0);
  });

  it("has no rate before anyone visited", () => {
    expect(goalRate(0, 0)).toBeNull();
  });
});

describe("analyticsGate", () => {
  const on = { enabled: true };
  it("closes on the plan before looking at the Environment", () => {
    expect(analyticsGate("unpaid", on)).toEqual({ gate: "unpaid" });
    expect(analyticsGate("self-host-off", on)).toEqual({ gate: "self-host-off" });
  });

  it("opens on a paid or self-host plan only where the Environment turned it on", () => {
    expect(analyticsGate("paid", on)).toEqual({ gate: "on", site: on });
    expect(analyticsGate("self-host-on", on)).toEqual({ gate: "on", site: on });
    expect(analyticsGate("paid", { enabled: false })).toEqual({ gate: "site-off" });
    expect(analyticsGate("paid", undefined)).toEqual({ gate: "site-off" });
  });
});

describe("page filter", () => {
  it("reads eq and neq on a path, and nothing else", () => {
    expect(parsePageFilter("eq:/pricing")).toEqual({ key: "page", op: "eq", value: "/pricing" });
    expect(parsePageFilter("neq:/")).toEqual({ key: "page", op: "neq", value: "/" });
    expect(parsePageFilter("like:/x")).toBeNull();
    expect(parsePageFilter("eq:pricing")).toBeNull();
    expect(parsePageFilter(undefined)).toBeNull();
  });
});

it("rounds views per visit to a tenth and survives no visits", () => {
  expect(viewsPerVisit(10, 3)).toBe(3.3);
  expect(viewsPerVisit(5, 0)).toBe(0);
});
