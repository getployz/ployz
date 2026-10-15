import { describe, expect, it } from "vitest";
import { dayEnd, dayStart, hourBuckets, retentionCutoff, retentionFloor } from "./local-time";

const iso = (dates: ReadonlyArray<Date>) => dates.map((date) => date.toISOString());

describe("retention", () => {
  it("clamps the floor to the last day of a shorter month", () => {
    expect(retentionFloor("UTC", Date.parse("2026-03-31T03:20:00Z"), 13).toISOString()).toBe("2025-02-28T00:00:00.000Z");
    expect(retentionFloor("UTC", Date.parse("2026-03-29T03:20:00Z"), 13).toISOString()).toBe("2025-02-28T00:00:00.000Z");
    expect(retentionFloor("UTC", Date.parse("2028-03-31T03:20:00Z"), 1).toISOString()).toBe("2028-02-29T00:00:00.000Z");
  });

  it("puts the cutoff a day before the UTC floor so every timezone's floor is at or after it", () => {
    const now = Date.parse("2026-11-01T03:20:00Z");
    expect(retentionCutoff(now).toISOString()).toBe("2025-09-30T00:00:00.000Z");
    expect(retentionFloor("Pacific/Kiritimati", now, 13).getTime()).toBeGreaterThanOrEqual(retentionCutoff(now).getTime());
  });
});

describe("hour buckets", () => {
  it("follows Lord Howe's half-hour spring forward without dropping local 03:00", () => {
    const day = "2026-10-04";
    const buckets = hourBuckets("Australia/Lord_Howe", dayStart("Australia/Lord_Howe", day), dayEnd("Australia/Lord_Howe", day));
    expect(iso(buckets.slice(0, 4))).toEqual([
      "2026-10-03T13:30:00.000Z",
      "2026-10-03T14:30:00.000Z",
      "2026-10-03T15:00:00.000Z",
      "2026-10-03T16:00:00.000Z",
    ]);
    expect(buckets).toHaveLength(24);
    expect(buckets.map((bucket) => bucket.getTime())).toContain(Date.parse("2026-10-03T16:00:00Z"));
  });

  it("follows Lord Howe's half-hour fall back", () => {
    const day = "2026-04-05";
    const buckets = hourBuckets("Australia/Lord_Howe", dayStart("Australia/Lord_Howe", day), dayEnd("Australia/Lord_Howe", day));
    expect(iso(buckets.slice(0, 4))).toEqual([
      "2026-04-04T13:00:00.000Z",
      "2026-04-04T14:00:00.000Z",
      "2026-04-04T14:30:00.000Z",
      "2026-04-04T15:30:00.000Z",
    ]);
    expect(buckets).toHaveLength(25);
  });
});
