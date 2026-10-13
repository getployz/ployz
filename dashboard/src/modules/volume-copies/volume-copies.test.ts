import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import {
  addableCopies, cardLine, hangingCopies, type MirrorCopy, type OffsiteCopy, type OldCopy, type Snapshot, traySpeech,
  type VolumeCopies,
} from "./volume-copies";

const today = (time: string) => new Date(`2026-10-12T${time}:00Z`);
const server = (name: string) => ({ id: name.padEnd(32, "0") as MachineId, name });
const snapshots = (n: number): Snapshot[] =>
  Array.from({ length: n }, (_, i) => ({ id: `s${i}`, at: today("05:00"), kind: "hourly", note: null, kept: false, bytes: 20e6 }));

const mirror = (refreshedAt: Date, extra: Partial<MirrorCopy> = {}): MirrorCopy => ({
  kind: "mirror", id: "web-2", server: server("web-2"), name: "pg-data-web-2", refreshedAt, skipped: null, staged: null, busy: null, ...extra,
});
const offsite: OffsiteCopy = {
  kind: "offsite", id: "acme-backups", bucket: "acme-backups", region: "eu-central-1", keep: 7, uploadedAt: today("07:04"), staged: null, busy: null,
};
const old: OldCopy = { kind: "old", id: "web-3", server: server("web-3"), name: "pg-data-old", since: null, bytes: 900e6, staged: null, busy: null };

const pgData = (elsewhere: VolumeCopies["elsewhere"], live: Partial<VolumeCopies["live"]> = {}): VolumeCopies => ({
  volume: { id: "v1", name: "pg-data" },
  live: { server: server("web-1"), restore: null, moveTo: null, busy: null, snapshots: snapshots(13), ...live },
  elsewhere,
});
const redisData: VolumeCopies = {
  volume: { id: "v2", name: "redis-data" },
  live: { server: server("web-2"), restore: null, moveTo: null, busy: null, snapshots: snapshots(4) },
  elsewhere: [],
};

describe("protected", () => {
  const now = today("14:04");
  const copies = pgData([mirror(today("14:00")), offsite, old]);

  it("hangs the mirror and offsite copy under the tray and keeps the old copy in the drawer", () => {
    expect(hangingCopies(copies).map((copy) => copy.kind)).toEqual(["mirror", "offsite"]);
    expect(hangingCopies(copies).map((copy) => cardLine(copy, now))).toEqual([
      { spin: false, words: "Mirror", age: "4 minutes ago", stale: false },
      { spin: false, words: "Offsite", age: "7 hours ago", stale: false },
    ]);
  });

  it("keeps the tray silent", () => {
    expect(traySpeech(copies)).toBeNull();
  });

  it("offers Add copy only for a kind still missing", () => {
    expect(addableCopies(copies, ["mirror", "offsite"])).toEqual([]);
    expect(addableCopies(pgData([mirror(today("14:00"))]), ["mirror", "offsite"])).toEqual(["offsite"]);
  });

  it("offers every copy kind to a volume with none", () => {
    expect(addableCopies(redisData, ["mirror", "offsite"])).toEqual(["mirror", "offsite"]);
  });

  it("shows a mirror Deploy has yet to add without an age", () => {
    const added = mirror(today("14:00"), { staged: "new", refreshedAt: null });
    expect(cardLine(added, now)).toEqual({ spin: false, words: "Mirror", age: null, stale: false });
  });

  it("drops the kind from a card staged for removal, leaving the badge", () => {
    expect(cardLine(mirror(today("14:00"), { staged: "removing" }), now).words).toBe("");
  });

  it("speaks a staged move on the tray", () => {
    expect(traySpeech(pgData([mirror(today("14:00"), { staged: "primary" })], { moveTo: server("web-2") })))
      .toEqual({ tone: "staged", words: "Moving to web-2" });
  });
});

describe("stale mirror", () => {
  const now = today("14:04");
  const behind = mirror(today("11:02"), { skipped: "failed" });

  it("marks the mirror card stale", () => {
    expect(cardLine(behind, now)).toEqual({ spin: false, words: "Mirror", age: "3 hours ago", stale: true });
  });

  it("is not behind at exactly an hour", () => {
    expect(cardLine(mirror(today("13:04")), now).stale).toBe(false);
  });
});

describe("failed migration", () => {
  const now = today("14:01");

  it("shows a fresh mirror as a minute old", () => {
    expect(cardLine(mirror(today("14:00")), now).age).toBe("1 minute ago");
    expect(cardLine(mirror(now), now).age).toBe("just now");
  });

  it("says Snapshotting on the tray while the deploy snapshots the volume", () => {
    expect(traySpeech(pgData([mirror(today("14:00"))], { busy: "Snapshotting" }))).toEqual({ tone: "busy", words: "Snapshotting" });
  });

  it("speaks the restore staged after the failure, ahead of anything else staged", () => {
    const copies = pgData([mirror(today("14:00"))], { restore: { label: "14:01" }, moveTo: server("web-2") });
    expect(traySpeech(copies)).toEqual({ tone: "staged", words: "Restore 14:01" });
  });

  it("shows what runs on a copy in place of its age", () => {
    expect(cardLine(mirror(today("14:00"), { busy: "Removing", staged: "removing" }), now)).toEqual({ spin: true, words: "Removing", age: null, stale: false });
  });
});
