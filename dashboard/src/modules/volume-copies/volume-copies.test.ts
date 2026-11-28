import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import {
  cardLine, elsewhereLine, hangingCopies, type MirrorCopy, type OldCopy, traySpeech,
  type VolumeCopies,
} from "./volume-copies";

const today = (time: string) => new Date(`2026-10-12T${time}:00Z`);
const server = (name: string) => ({ id: name.padEnd(32, "0") as MachineId, name });

const mirror = (refreshedAt: Date, extra: Partial<MirrorCopy> = {}): MirrorCopy => ({
  kind: "mirror", id: "web-2", server: server("web-2"), name: "pg-data-web-2", refreshedAt, failed: false, staged: null, busy: null, ...extra,
});
const old: OldCopy = { kind: "old", id: "web-3", server: server("web-3"), name: "pg-data-old", since: null, bytes: 900e6, staged: null, busy: null };

const pgData = (elsewhere: VolumeCopies["elsewhere"], live: Partial<VolumeCopies["live"]> = {}): VolumeCopies => ({
  volume: { id: "v1", name: "pg-data" },
  live: { server: { ...server("web-1"), state: "live" }, restore: null, moveTo: null, busy: null, ...live },
  elsewhere,
  unheard: [],
});
describe("protected", () => {
  const now = today("14:04");
  const copies = pgData([mirror(today("14:00")), old]);

  it("hangs the mirror under the tray and keeps the old copy in the drawer", () => {
    expect(hangingCopies(copies).map((copy) => copy.kind)).toEqual(["mirror"]);
    expect(hangingCopies(copies).map((copy) => cardLine(copy, now))).toEqual([
      { spin: false, words: "Mirror", age: "4 minutes ago", stale: false },
    ]);
  });

  it("keeps the tray silent", () => {
    expect(traySpeech(copies)).toBeNull();
  });

  it("shows a mirror Deploy has yet to add without an age", () => {
    const added = mirror(today("14:00"), { staged: "new", refreshedAt: null });
    expect(cardLine(added, now)).toEqual({ spin: false, words: "Mirror", age: null, stale: false });
  });

  it("warns on a live mirror that has never refreshed", () => {
    expect(cardLine(mirror(today("14:00"), { refreshedAt: null }), now)).toEqual({ spin: false, words: "Mirror", age: "Not yet", stale: true });
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
  const behind = mirror(today("11:02"), { failed: true });

  it("marks the mirror card stale", () => {
    expect(cardLine(behind, now)).toEqual({ spin: false, words: "Mirror", age: "3 hours ago", stale: true });
  });

  it("is not behind at exactly an hour", () => {
    expect(cardLine(mirror(today("13:04")), now).stale).toBe(false);
  });
});

describe("elsewhereLine", () => {
  const silent = { ...server("web-2"), heard: "silent" as const };

  it("says nothing is outside Live only once every Server reported", () => {
    expect(elsewhereLine(pgData([]))).toBe("Nothing outside web-1. If web-1 dies, pg-data goes with it.");
    expect(elsewhereLine(pgData([mirror(today("13:55"))]))).toBeNull();
  });

  it("says a silent Server keeps Copies from knowing what is outside Live, listed copies or not", () => {
    expect(elsewhereLine({ ...pgData([]), unheard: [silent] })).toBe("web-2 is not answering, so Copies can't say what is outside web-1.");
    expect(elsewhereLine({ ...pgData([old]), unheard: [silent] })).toBe("web-2 is not answering, so a copy there won't show until it reports.");
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
