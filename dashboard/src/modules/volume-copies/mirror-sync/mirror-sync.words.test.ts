import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { MirrorCopy } from "#/modules/volume-copies/volume-copies";
import { mirrorLast, skippedWords } from "./mirror-sync.words";

const now = new Date("2026-10-12T16:00:00Z");
const minutesAgo = (minutes: number) => new Date(now.getTime() - minutes * 60_000);
const mirror = (refreshedAt: Date | null, extra: Partial<MirrorCopy> = {}): MirrorCopy => ({
  kind: "mirror", id: "web-2", server: { id: "web-2".padEnd(32, "0") as MachineId, name: "web-2" }, name: "pg-data-web-2",
  refreshedAt, failed: false, staged: null, busy: null, ...extra,
});

describe("mirrorLast", () => {
  it("spins on what runs now, whatever the age", () => {
    expect(mirrorLast(mirror(minutesAgo(200), { busy: "Refreshing", failed: true }), now))
      .toEqual({ spin: true, words: "Refreshing", stale: false });
  });

  it("says just now for a round that landed this minute", () => {
    expect(mirrorLast(mirror(minutesAgo(0.5)), now)).toEqual({ spin: false, words: "just now", stale: false });
  });

  it("gives the age, and warns once it passes an hour", () => {
    expect(mirrorLast(mirror(minutesAgo(4)), now)).toEqual({ spin: false, words: "4 minutes ago", stale: false });
    expect(mirrorLast(mirror(minutesAgo(135)), now)).toMatchObject({ spin: false, stale: true });
  });

  it("says Not yet for a mirror with no point, warning only once it is live", () => {
    expect(mirrorLast(mirror(null), now)).toMatchObject({ words: "Not yet", stale: true });
    expect(mirrorLast(mirror(minutesAgo(4), { staged: "new" }), now)).toMatchObject({ words: "Not yet", stale: false });
  });
});

describe("skippedWords", () => {
  it("says the last refresh failed and how many writes a failover loses, once the mirror is behind", () => {
    expect(skippedWords(mirror(minutesAgo(135), { failed: true }), now)).toMatch(/^The last refresh failed\..*loses 2 hours of writes\.$/);
    expect(skippedWords(mirror(minutesAgo(61), { failed: true }), now)).toMatch(/loses 1 hour of writes\.$/);
    expect(skippedWords(mirror(minutesAgo(3 * 1440), { failed: true }), now)).toMatch(/loses 3 days of writes\.$/);
  });

  it("says nothing while rounds land, a fresh refresh failed, or a round runs", () => {
    expect(skippedWords(mirror(minutesAgo(135)), now)).toBeNull();
    expect(skippedWords(mirror(minutesAgo(20), { failed: true }), now)).toBeNull();
    expect(skippedWords(mirror(minutesAgo(135), { failed: true, busy: "Refreshing" }), now)).toBeNull();
    expect(skippedWords(mirror(null, { failed: true }), now)).toBeNull();
  });
});
