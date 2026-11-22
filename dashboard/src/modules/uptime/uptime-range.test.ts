import { describe, expect, it } from "vitest";
import { checkWindow, pointSource } from "./uptime-range";

const NOW = Date.parse("2026-10-11T12:30:00.000Z");
const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

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
