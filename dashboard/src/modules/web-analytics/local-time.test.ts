import { describe, expect, it } from "vitest";
import { LONGEST_RETENTION_MONTHS } from "./model";
import { dayEnd, dayStart, hourBuckets, localDay, retentionCutoff, retentionFloor } from "./local-time";

const iso = (dates: ReadonlyArray<Date>) => dates.map((date) => date.toISOString());

describe("retention", () => {
  it("clamps the floor to the last day of a shorter month", () => {
    expect(retentionFloor("UTC", Date.parse("2026-03-31T03:20:00Z"), 13).toISOString()).toBe("2025-02-28T00:00:00.000Z");
    expect(retentionFloor("UTC", Date.parse("2026-03-29T03:20:00Z"), 13).toISOString()).toBe("2025-02-28T00:00:00.000Z");
    expect(retentionFloor("UTC", Date.parse("2028-03-31T03:20:00Z"), 1).toISOString()).toBe("2028-02-29T00:00:00.000Z");
  });

  it("puts the cutoff a day before the UTC floor of yesterday", () => {
    const now = Date.parse("2026-11-01T03:20:00Z");
    expect(retentionCutoff(now).toISOString()).toBe("2025-09-29T00:00:00.000Z");
    expect(retentionFloor("Pacific/Kiritimati", now, 13).getTime()).toBeGreaterThanOrEqual(retentionCutoff(now).getTime());
  });

  it("keeps Honolulu's floor on March 1 UTC, when its local date is still in February", () => {
    const now = Date.parse("2026-03-01T03:20:00Z");
    expect(retentionFloor("Pacific/Honolulu", now, 13).toISOString()).toBe("2025-01-28T10:00:00.000Z");
    expect(retentionCutoff(now).getTime()).toBeLessThanOrEqual(Date.parse("2025-01-28T10:00:00Z"));
  });

  it("puts the cutoff at or before every timezone's floor around every month boundary", () => {
    const zones = ["Etc/GMT+12", "Pacific/Pago_Pago", "Pacific/Honolulu", "America/St_Johns", "UTC", "Asia/Kolkata",
      "Australia/Lord_Howe", "Pacific/Chatham", "Pacific/Kiritimati", "Etc/GMT-14"];
    const times = ["00:00", "03:20", "10:00", "11:59", "12:00", "13:30", "23:59"];
    for (let month = 0; month < 36; month++) {
      for (const date of [-2, -1, 0, 1, 2, 3]) {
        for (const time of times) {
          const day = new Date(Date.UTC(2027, month, 1 + date)).toISOString().slice(0, 10);
          const now = Date.parse(`${day}T${time}:00Z`);
          const cutoff = retentionCutoff(now);
          for (const zone of zones) {
            const floor = retentionFloor(zone, now, LONGEST_RETENTION_MONTHS);
            expect(floor.getTime(), `${zone} at ${day}T${time}`).toBeGreaterThanOrEqual(cutoff.getTime());
            expect(localDay(zone, floor) >= cutoff.toISOString().slice(0, 10), `${zone} day at ${day}T${time}`).toBe(true);
          }
        }
      }
    }
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
