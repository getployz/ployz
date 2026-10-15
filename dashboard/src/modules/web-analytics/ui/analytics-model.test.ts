import { describe, expect, it } from "vitest";
import { customRange, isRangeSpec, parsePageFilter, partialDays, resolveRange, viewsPerVisit } from "./analytics-model";

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

  it("refuses a spec it cannot read", () => {
    expect(isRangeSpec("7d")).toBe(true);
    expect(isRangeSpec(customRange("2026-10-02", "2026-10-01"))).toBe(false);
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
