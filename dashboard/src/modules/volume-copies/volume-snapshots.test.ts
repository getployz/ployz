import { describe, expect, it } from "vitest";
import type { UserSnapshot } from "@ployz/sdk";
import { lossWords, relWords, sizeWords, snapshotClock, snapshotFromWire, snapshotsServed } from "./volume-snapshots";

const NOW = new Date("2026-10-12T12:00:00Z");
const ago = (minutes: number) => new Date(NOW.getTime() - minutes * 60_000);
const machine = (capabilities: string[]) => ({ id: "w1", capabilities });

describe("snapshotsServed", () => {
  it("needs both snapshot capabilities on the writer", () => {
    expect(snapshotsServed([machine(["ployz.volume.snapshot.v1", "ployz.volume.snapshot.list.v1"])], "w1")).toBe(true);
    expect(snapshotsServed([machine(["ployz.volume.snapshot.v1"])], "w1")).toBe(false);
    expect(snapshotsServed([machine(["ployz.volume.snapshot.list.v1"])], "w1")).toBe(false);
  });

  it("is off without a writer, or when the writer is another Server", () => {
    const both = machine(["ployz.volume.snapshot.v1", "ployz.volume.snapshot.list.v1"]);
    expect(snapshotsServed([both], null)).toBe(false);
    expect(snapshotsServed([both], "w2")).toBe(false);
  });
});

describe("snapshotFromWire", () => {
  it("reads written bytes as the size and an empty note as none", () => {
    const wire: UserSnapshot = {
      id: "s1", label: { kind: "manual", note: "" }, kept: true, created_unix_seconds: NOW.getTime() / 1000,
      written_bytes: 20e6, referenced_bytes: 900e6, loses_bytes: 5e6,
    };
    expect(snapshotFromWire(wire)).toEqual({ id: "s1", at: NOW, kind: "manual", note: null, kept: true, bytes: 20e6, losesBytes: 5e6 });
  });
});

describe("snapshotClock", () => {
  const clock = snapshotClock("UTC");

  it("groups by Today, Yesterday, then the date, newest first", () => {
    const snaps = [ago(10), ago(60), ago(60 * 24), ago(60 * 24 * 3)].map((at, i) =>
      ({ id: `s${i}`, at, kind: "hourly" as const, note: null, kept: false, bytes: 0, losesBytes: 0 }));
    expect(clock.groups(snaps, NOW).map((day) => [day.day, day.snapshots.length])).toEqual([["Today", 2], ["Yesterday", 1], ["Oct 9", 1]]);
  });

  it("names the date only before today", () => {
    expect(clock.when(ago(45), NOW)).toBe("11:15");
    expect(clock.when(ago(60 * 24), NOW)).toBe("Oct 11 12:00");
  });
});

describe("words", () => {
  it("sizes in MB under a GB", () => {
    expect(sizeWords(20e6)).toBe("20 MB");
    expect(sizeWords(1.2e9)).toBe("1.2 GB");
  });

  it("says nothing is lost when nothing was written", () => {
    expect(lossWords({ at: ago(180), losesBytes: 0 }, NOW)).toBeNull();
    expect(lossWords({ at: ago(180), losesBytes: 1.2e9 }, NOW)).toBe("1.2 GB · 3 hours of writes");
  });

  it("says how long ago", () => {
    expect(relWords(ago(0.5), NOW)).toBe("just now");
    expect(relWords(ago(120), NOW)).toBe("2 hours ago");
  });
});
