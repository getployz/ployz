import { describe, expect, it } from "vitest";
import { checkWindow, edgeText, parseEdge, pointSource, rangeLabel, resolveRange } from "./uptime-range";

const NOW = Date.parse("2026-10-11T12:30:00.000Z");
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const NY = "America/New_York";

describe("parseEdge", () => {
  it("reads offsets back from now in any unit spelling, with or without ago", () => {
    expect(parseEdge("15m", NY, NOW)).toBe("15m");
    expect(parseEdge(" 15 min ago ", NY, NOW)).toBe("15m");
    expect(parseEdge("2 hours ago", NY, NOW)).toBe("2h");
    expect(parseEdge("1 Week", NY, NOW)).toBe("1w");
    expect(parseEdge("-5m", NY, NOW)).toBe("5m");
    expect(parseEdge("90 minutes ago", NY, NOW)).toBe("90m");
    expect(parseEdge("0m", NY, NOW)).toBe("now");
    expect(parseEdge("Now", NY, NOW)).toBe("now");
    expect(parseEdge("3 fortnights", NY, NOW)).toBeNull();
  });

  it("reads today and yesterday as midnight where the person is", () => {
    expect(parseEdge("today", NY, NOW)).toBe("2026-10-11T04:00:00.000Z");
    expect(parseEdge("yesterday", NY, NOW)).toBe("2026-10-10T04:00:00.000Z");
    expect(parseEdge("today", "Asia/Tokyo", NOW)).toBe("2026-10-10T15:00:00.000Z");
    expect(parseEdge("today", "Asia/Tokyo", Date.parse("2026-10-11T16:00:00.000Z"))).toBe("2026-10-11T15:00:00.000Z");
  });

  it("reads dates and times as wall clock in the person's zone", () => {
    expect(parseEdge("2023-12-25", NY, NOW)).toBe("2023-12-25T05:00:00.000Z");
    expect(parseEdge("2026-10-11 14:30", NY, NOW)).toBe("2026-10-11T18:30:00.000Z");
    expect(parseEdge("2026-10-11T9:05:30", "UTC", NOW)).toBe("2026-10-11T09:05:30.000Z");
  });

  it("reads wall times across daylight saving changes", () => {
    expect(parseEdge("2026-03-08 01:30", NY, NOW)).toBe("2026-03-08T06:30:00.000Z");
    expect(parseEdge("2026-03-08 02:30", NY, NOW)).toBe("2026-03-08T07:30:00.000Z");
    expect(parseEdge("2026-03-08 03:30", NY, NOW)).toBe("2026-03-08T07:30:00.000Z");
    expect(parseEdge("2026-11-01 01:30", NY, NOW)).toBe("2026-11-01T05:30:00.000Z");
    expect(parseEdge("2026-11-01 03:00", NY, NOW)).toBe("2026-11-01T08:00:00.000Z");
  });

  it("reads instants with their own zone", () => {
    expect(parseEdge("2026-10-11T14:00:00Z", NY, NOW)).toBe("2026-10-11T14:00:00.000Z");
    expect(parseEdge("2026-10-11T14:00+02:00", NY, NOW)).toBe("2026-10-11T12:00:00.000Z");
  });

  it("names nothing for dates that do not exist or text that is not a time", () => {
    for (const input of ["2026-02-30", "2026-10-11 24:00", "2026-10-11 12:60", "soon", "", "15", "in a bit"]) {
      expect(parseEdge(input, NY, NOW)).toBeNull();
    }
  });

  it("reads back what From and Until show", () => {
    for (const edge of ["now", "15m", "2026-10-11T18:30:00.000Z", "2026-03-08T07:30:00.000Z"]) {
      expect(parseEdge(edgeText(edge, NY), NY, NOW)).toBe(edge);
    }
    expect(edgeText("2026-10-11T18:30:00.000Z", NY)).toBe("2026-10-11 14:30");
    expect(edgeText("15m", NY)).toBe("15m ago");
  });
});

describe("rangeLabel", () => {
  it("names a range back from now by its quick range or offset, and others by their edges", () => {
    expect(rangeLabel({ from: "15m", to: "now" }, NY)).toBe("Last 15 min");
    expect(rangeLabel({ from: "2h", to: "now" }, NY)).toBe("Last 2h");
    expect(rangeLabel({ from: "2026-10-10T18:00:00.000Z", to: "now" }, NY)).toBe("Oct 10, 14:00 – now");
    expect(rangeLabel({ from: "2d", to: "1d" }, NY)).toBe("2d ago – 1d ago");
  });
});

describe("resolveRange", () => {
  it("never runs past now or backwards", () => {
    expect(resolveRange({ from: "1h", to: "now" }, NOW)).toEqual({ from: NOW - HOUR, to: NOW });
    expect(resolveRange({ from: "1h", to: "2026-10-12T00:00:00.000Z" }, NOW)).toEqual({ from: NOW - HOUR, to: NOW });
    expect(resolveRange({ from: "1h", to: "2h" }, NOW)).toEqual({ from: NOW - 2 * HOUR, to: NOW - 2 * HOUR });
  });
});

describe("checkWindow", () => {
  it("covers at least the last 30 minutes of a range", () => {
    expect(checkWindow({ from: NOW - 5 * MINUTE, to: NOW })).toEqual({ from: NOW - 30 * MINUTE, to: NOW });
    expect(checkWindow({ from: NOW - HOUR, to: NOW })).toEqual({ from: NOW - HOUR, to: NOW });
  });
});

describe("pointSource", () => {
  it("reads raw probes up to a day within raw retention, then hourly, then 6-hour buckets", () => {
    expect(pointSource({ from: NOW - DAY, to: NOW }, NOW)).toBe("raw");
    expect(pointSource({ from: NOW - 40 * DAY, to: NOW - 39 * DAY }, NOW)).toBe("hourly");
    expect(pointSource({ from: NOW - 7 * DAY, to: NOW }, NOW)).toBe("hourly");
    expect(pointSource({ from: NOW - 30 * DAY, to: NOW }, NOW)).toBe("sixHourly");
  });
});
