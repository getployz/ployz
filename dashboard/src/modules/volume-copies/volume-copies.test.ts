import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import {
  cardLine, hangingCopies, type MirrorCopy, type OldCopy, traySpeech, unheardReason,
  type VolumeCopies,
} from "./volume-copies";

const today = (time: string) => new Date(`2026-10-12T${time}:00Z`);
const server = (name: string) => ({ id: name.padEnd(32, "0") as MachineId, name });

const mirror = (refreshedAt: Date, extra: Partial<MirrorCopy> = {}): MirrorCopy => ({
  kind: "mirror", id: "web-2", server: server("web-2"), name: "pg-data-web-2", refreshedAt, skipped: null, lastReported: false, staged: null, busy: null, ...extra,
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

  it("words a mirror known only from its Server's last report as last reported, staged or not", () => {
    expect(cardLine(mirror(today("14:00"), { lastReported: true }), now).words).toBe("Last reported");
    expect(cardLine(mirror(today("14:00"), { lastReported: true, staged: "primary" }), now).words).toBe("Last reported");
    expect(cardLine(mirror(today("14:00"), { lastReported: true, staged: "removing" }), now).words).toBe("");
  });

  it("gives a last-reported mirror no age, never Not yet, since nothing says when its Server reported it", () => {
    expect(cardLine(mirror(today("14:00"), { lastReported: true, refreshedAt: null, skipped: "silent" }), now))
      .toEqual({ spin: false, words: "Last reported", age: null, stale: false });
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

describe("unheard mirror", () => {
  const now = today("14:04");

  it("says why a mirror's Server didn't report it however fresh its age, which stays the last match", () => {
    const silent = mirror(today("13:55"), { skipped: "silent" });
    expect(unheardReason(silent)).toBe("Not answering");
    expect(cardLine(silent, now)).toMatchObject({ age: "9 minutes ago", stale: false });
    expect(unheardReason(mirror(today("13:55"), { skipped: "waiting" }))).toBe("Waiting for its report");
    expect(unheardReason(mirror(today("13:55"), { skipped: "lacks" }))).toBe("Can't report this copy");
    expect(unheardReason(mirror(today("13:55"), { skipped: "outdated" }))).toBe("Needs a newer Ployz");
  });

  it("gives no reason for a mirror whose Server answered, or while a run is on it", () => {
    expect(unheardReason(mirror(today("13:55")))).toBeNull();
    expect(unheardReason(mirror(today("13:55"), { skipped: "failed" }))).toBeNull();
    expect(unheardReason(mirror(today("13:55"), { skipped: "silent", busy: "Refreshing" }))).toBeNull();
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
