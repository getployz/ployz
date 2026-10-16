import { describe, expect, it } from "vitest";
import { isHourStart } from "../local-time";
import {
  analyticsGate,
  customRange,
  formatMs,
  formatRate,
  goalRate,
  isRangeSpec,
  parsePageFilter,
  partialDays,
  resolveRange,
  resolveWindow,
  sortRoutes,
  viewsPerVisit,
} from "./analytics-model";

const iso = (range: { start: Date; end: Date }) => [range.start.toISOString(), range.end.toISOString()];
const NOW = Date.parse("2026-10-15T09:20:00Z");

describe("resolveWindow", () => {
  it("covers at least the window in whole hours through the end of the current one", () => {
    expect(iso(resolveWindow("1h", "UTC", NOW))).toEqual(["2026-10-15T08:00:00.000Z", "2026-10-15T10:00:00.000Z"]);
    expect(iso(resolveWindow("7d", "UTC", NOW))).toEqual(["2026-10-08T09:00:00.000Z", "2026-10-15T10:00:00.000Z"]);
  });

  it("starts on the site's own hour in a half-hour zone", () => {
    expect(iso(resolveWindow("24h", "Asia/Kolkata", NOW))).toEqual(["2026-10-14T08:30:00.000Z", "2026-10-15T09:30:00.000Z"]);
  });
});

describe("sortRoutes", () => {
  const row = (route: string, requests: number, errors: number, p95: number) => ({ route, requests, errors, p50: 0, p95, p99: 0 });
  const routes = [row("/a", 10, 5, 30), row("/b", 100, 1, 10), row("/c", 50, 0, 900)];
  const order = (sorted: ReadonlyArray<{ route: string }>) => sorted.map((route) => route.route);

  it("orders by the asked column in either direction", () => {
    expect(order(sortRoutes(routes, { key: "requests", descending: true }))).toEqual(["/b", "/c", "/a"]);
    expect(order(sortRoutes(routes, { key: "p95", descending: true }))).toEqual(["/c", "/a", "/b"]);
    expect(order(sortRoutes(routes, { key: "route", descending: false }))).toEqual(["/a", "/b", "/c"]);
  });

  it("ranks by failure share, not failure count", () => {
    expect(order(sortRoutes(routes, { key: "errorRate", descending: true }))).toEqual(["/a", "/b", "/c"]);
  });
});

describe("request formats", () => {
  it("keeps a rare failure visible and reads durations in ms then seconds", () => {
    expect([formatRate(0, 10), formatRate(1, 1000), formatRate(1, 3)]).toEqual(["0%", "0.1%", "33%"]);
    expect([formatMs(12.4), formatMs(1234), formatMs(12_345)]).toEqual(["12ms", "1.23s", "12.3s"]);
  });
});

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
  it("marks days with pageviews no salt could count, then today", () => {
    const days = ["2026-10-12", "2026-10-13", "2026-10-14", "2026-10-15"];
    expect(partialDays(days, { today: "2026-10-15", noSalt: ["2026-10-13"] })).toEqual([
      { day: "2026-10-13", reason: "no-salt" },
      { day: "2026-10-15", reason: "today" },
    ]);
  });

  it("names a missing salt over today when today has both", () => {
    expect(partialDays(["2026-10-15"], { today: "2026-10-15", noSalt: ["2026-10-15"] })).toEqual([{ day: "2026-10-15", reason: "no-salt" }]);
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
    expect(analyticsGate("self-host-off", on)).toEqual({ gate: "self-host-off" });
  });

  it("opens on Cloud or a self-host plan only where the Environment turned it on", () => {
    expect(analyticsGate("cloud", on)).toEqual({ gate: "on", site: on });
    expect(analyticsGate("self-host-on", on)).toEqual({ gate: "on", site: on });
    expect(analyticsGate("cloud", { enabled: false })).toEqual({ gate: "site-off" });
    expect(analyticsGate("cloud", undefined)).toEqual({ gate: "site-off" });
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
