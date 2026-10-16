import { describe, expect, it } from "vitest";
import { dayEnd, dayStart, hourBuckets, localDay, retentionCutoff, retentionFloor } from "./local-time";

const iso = (dates: ReadonlyArray<Date>) => dates.map((date) => date.toISOString());

describe("retention", () => {
  it("keeps today and the 29 local days before it", () => {
    expect(retentionFloor("UTC", Date.parse("2026-03-31T03:20:00Z")).toISOString()).toBe("2026-03-02T00:00:00.000Z");
    expect(retentionFloor("Pacific/Honolulu", Date.parse("2026-03-01T03:20:00Z")).toISOString()).toBe("2026-01-30T10:00:00.000Z");
  });

  it("puts the cutoff a day before the UTC floor", () => {
    expect(retentionCutoff(Date.parse("2026-11-01T03:20:00Z")).toISOString()).toBe("2026-10-02T00:00:00.000Z");
  });

  it("puts the cutoff at or before every timezone's floor", () => {
    const zones = ["Etc/GMT+12", "Pacific/Pago_Pago", "Pacific/Honolulu", "America/St_Johns", "UTC", "Asia/Kolkata",
      "Australia/Lord_Howe", "Pacific/Chatham", "Pacific/Kiritimati", "Etc/GMT-14"];
    const times = ["00:00", "03:20", "10:00", "11:59", "12:00", "13:30", "23:59"];
    for (let date = 0; date < 400; date += 3) {
      for (const time of times) {
        const day = new Date(Date.UTC(2027, 0, 1 + date)).toISOString().slice(0, 10);
        const now = Date.parse(`${day}T${time}:00Z`);
        const cutoff = retentionCutoff(now);
        for (const zone of zones) {
          const floor = retentionFloor(zone, now);
          expect(floor.getTime(), `${zone} at ${day}T${time}`).toBeGreaterThanOrEqual(cutoff.getTime());
          expect(localDay(zone, floor) >= cutoff.toISOString().slice(0, 10), `${zone} day at ${day}T${time}`).toBe(true);
        }
      }
    }
  });
});

const localHourOf = (instant: Date) =>
  new Intl.DateTimeFormat("en-AU", { timeZone: "Australia/Lord_Howe", hour: "2-digit", hourCycle: "h23", day: "2-digit" }).format(instant);

describe("hour buckets", () => {
  it("follows Lord Howe's half-hour spring forward without dropping local 03:00", () => {
    const day = "2026-10-04";
    const buckets = hourBuckets("Australia/Lord_Howe", dayStart("Australia/Lord_Howe", day), dayEnd("Australia/Lord_Howe", day));
    expect(iso(buckets.slice(0, 4))).toEqual([
      "2026-10-03T13:30:00.000Z",
      "2026-10-03T14:30:00.000Z",
      "2026-10-03T15:30:00.000Z",
      "2026-10-03T16:00:00.000Z",
    ]);
    expect(buckets).toHaveLength(24);
    for (const bucket of buckets) expect(localHourOf(bucket), bucket.toISOString()).toBe(localHourOf(new Date(bucket.getTime() + 60_000)));
    expect(buckets.map((bucket) => bucket.getTime())).toContain(Date.parse("2026-10-03T16:00:00Z"));
  });

  it("follows Lord Howe's half-hour fall back", () => {
    const day = "2026-04-05";
    const buckets = hourBuckets("Australia/Lord_Howe", dayStart("Australia/Lord_Howe", day), dayEnd("Australia/Lord_Howe", day));
    expect(iso(buckets.slice(0, 4))).toEqual([
      "2026-04-04T13:00:00.000Z",
      "2026-04-04T14:00:00.000Z",
      "2026-04-04T15:00:00.000Z",
      "2026-04-04T15:30:00.000Z",
    ]);
    expect(buckets).toHaveLength(25);
    for (const bucket of buckets) expect(localHourOf(bucket), bucket.toISOString()).toBe(localHourOf(new Date(bucket.getTime() + 60_000)));
  });
});
