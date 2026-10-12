import { describe, expect, it } from "vitest";
import type { Snapshot } from "./volume-copies";
import {
  DEFAULT_POLICY, type SnapshotPolicy, clampCount, deploySnapshotId, deploysWords, dueAt, keepSpanWords, pruneIds, scheduleWords,
  scheduledSnapshotId, tickOf,
} from "./snapshot-schedule";

const snap = (id: string, kind: Snapshot["kind"], options: { kept?: boolean; bytes?: number } = {}) =>
  ({ id, kind, kept: options.kept ?? false, bytes: options.bytes ?? 1e6 });
const policy = (overrides: Partial<SnapshotPolicy>): SnapshotPolicy => ({ ...DEFAULT_POLICY, ...overrides });
const WIRE_ID = /^[a-z0-9-]{1,64}$/;

describe("scheduledSnapshotId", () => {
  const tick = new Date("2026-10-12T13:00:00Z");

  it("names the same snapshot for a retried tick, and a new one for the next", () => {
    expect(scheduledSnapshotId("hour", "vol-1", tick)).toBe(scheduledSnapshotId("hour", "vol-1", new Date(tick)));
    expect(scheduledSnapshotId("hour", "vol-1", tick)).not.toBe(scheduledSnapshotId("hour", "vol-1", new Date("2026-10-12T14:00:00Z")));
  });

  it("differs per Volume and per cadence, and fits the wire's id rule", () => {
    expect(scheduledSnapshotId("hour", "vol-1", tick)).not.toBe(scheduledSnapshotId("hour", "vol-2", tick));
    expect(scheduledSnapshotId("hour", "vol-1", tick)).not.toBe(scheduledSnapshotId("day", "vol-1", tick));
    expect(scheduledSnapshotId("day", `Vol_${"x".repeat(200)}`, tick)).toMatch(WIRE_ID);
  });

  it("rounds any time in the hour down to its tick", () => {
    expect(tickOf(new Date("2026-10-12T13:59:59.999Z"))).toEqual(tick);
    expect(tickOf(new Date("2026-10-12T13:00:00Z"))).toEqual(tick);
  });
});

describe("deploySnapshotId", () => {
  it("is stable per deployment, separate for before and after, and wire safe", () => {
    expect(deploySnapshotId("before", "dep-1")).toBe(deploySnapshotId("before", "dep-1"));
    expect(deploySnapshotId("before", "dep-1")).not.toBe(deploySnapshotId("after", "dep-1"));
    expect(deploySnapshotId("before", "dep-1")).not.toBe(deploySnapshotId("before", "dep-2"));
    expect(deploySnapshotId("after", "Dep.UPPER/1".repeat(20))).toMatch(WIRE_ID);
  });
});

describe("dueAt", () => {
  it("runs hourly every tick and daily only at midnight UTC", () => {
    const midnight = new Date("2026-10-12T00:00:00Z");
    const noon = new Date("2026-10-12T12:00:00Z");
    expect([dueAt("hour", midnight), dueAt("hour", noon)]).toEqual([true, true]);
    expect([dueAt("day", midnight), dueAt("day", noon)]).toEqual([true, false]);
    expect(dueAt("off", midnight)).toBe(false);
  });
});

describe("pruneIds", () => {
  it("drops scheduled snapshots past keep, oldest first, and never a kept one", () => {
    const list = [snap("h4", "hourly"), snap("h3", "hourly", { kept: true }), snap("h2", "hourly"), snap("h1", "hourly")];
    expect(pruneIds(list, policy({ every: "hour", keep: 1 }))).toEqual(["h2", "h1"]);
  });

  it("counts hourly and daily together and leaves manual snapshots alone", () => {
    const list = [snap("d2", "daily"), snap("m1", "manual"), snap("h1", "hourly"), snap("d1", "daily")];
    expect(pruneIds(list, policy({ every: "day", keep: 2 }))).toEqual(["d1"]);
  });

  it("keeps every scheduled snapshot while the schedule is off", () => {
    const list = [snap("h2", "hourly"), snap("h1", "hourly")];
    expect(pruneIds(list, policy({ every: "off", keep: 1 }))).toEqual([]);
  });

  it("drops deploy snapshots past keepDeploys, counting Before and After together", () => {
    const list = [snap("a2", "after_deploy"), snap("b2", "before_deploy"), snap("a1", "after_deploy", { kept: true }), snap("b1", "before_deploy")];
    expect(pruneIds(list, policy({ keepDeploys: 1 }))).toEqual(["b2", "b1"]);
  });

  it("drops the oldest automatic snapshots until the rest fit the cap, kept and manual ones included in the total", () => {
    const list = [
      snap("h3", "hourly", { bytes: 0.1e9 }), snap("m1", "manual", { bytes: 0.5e9 }), snap("h2", "hourly", { bytes: 0.4e9 }),
      snap("k1", "before_deploy", { kept: true, bytes: 0.3e9 }), snap("h1", "hourly", { bytes: 0.4e9 }),
    ];
    expect(pruneIds(list, policy({ every: "hour", keep: 168, capGb: 1 }))).toEqual(["h2", "h1"]);
  });

  it("stops at kept snapshots when they alone pass the cap", () => {
    const list = [snap("k1", "hourly", { kept: true, bytes: 3e9 }), snap("h1", "hourly", { bytes: 1e6 })];
    expect(pruneIds(list, policy({ every: "hour", keep: 168, capGb: 1 }))).toEqual(["h1"]);
  });
});

describe("words", () => {
  it("reads the policy the way the prototype does", () => {
    expect(deploysWords(DEFAULT_POLICY)).toBe("Off");
    expect(deploysWords(policy({ before: true, after: true, keepDeploys: 10 }))).toBe("Before and after · 10 kept");
    expect(deploysWords(policy({ after: true }))).toBe("After · 5 kept");
    expect(scheduleWords(DEFAULT_POLICY)).toBe("Off");
    expect(scheduleWords(policy({ every: "day", keep: 7 }))).toBe("Daily · 7 kept");
    expect(keepSpanWords(policy({ every: "hour", keep: 24 }))).toBe("1 day");
    expect(keepSpanWords(policy({ every: "day", keep: 7 }))).toBe("7 days");
  });

  it("clamps counts to the stepper limits", () => {
    expect([clampCount("keep", 0), clampCount("keep", 200), clampCount("capGb", 21), clampCount("keepDeploys", 3)]).toEqual([1, 168, 20, 3]);
  });
});
