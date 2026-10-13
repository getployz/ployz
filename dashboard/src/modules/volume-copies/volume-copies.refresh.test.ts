import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { MirrorCopy } from "./volume-copies";
import { mirrorLast, skippedWords } from "./volume-copies.refresh";

const now = new Date("2026-10-12T16:00:00Z");
const minutesAgo = (minutes: number) => new Date(now.getTime() - minutes * 60_000);
const mirror = (refreshedAt: Date | null, extra: Partial<MirrorCopy> = {}): MirrorCopy => ({
  kind: "mirror", id: "web-2", server: { id: "web-2".padEnd(32, "0") as MachineId, name: "web-2" }, name: "pg-data-web-2",
  refreshedAt, skipped: null, staged: null, busy: null, ...extra,
});

describe("mirrorLast", () => {
  it("spins on what runs now, whatever the age", () => {
    expect(mirrorLast(mirror(minutesAgo(200), { busy: "Refreshing", skipped: "silent" }), now))
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
  it("says the mirror is not answering and how many writes a failover loses", () => {
    expect(skippedWords(mirror(minutesAgo(135), { skipped: "silent" }), now)).toBe(
      "web-2 is not answering. Ployz retries every 15 minutes. Until it catches up, a failover loses 2 hours of writes.",
    );
    expect(skippedWords(mirror(minutesAgo(61), { skipped: "silent" }), now)).toMatch(/loses 1 hour of writes\.$/);
    expect(skippedWords(mirror(minutesAgo(3 * 1440), { skipped: "silent" }), now)).toMatch(/loses 3 days of writes\.$/);
  });

  it("says the last refresh failed when the mirror's Server answers", () => {
    expect(skippedWords(mirror(minutesAgo(135), { skipped: "failed" }), now)).toBe(
      "The last refresh failed. Ployz retries every 15 minutes. Until it catches up, a failover loses 2 hours of writes.",
    );
  });

  it("names no time when rounds skipped the Volume before its Server went silent", () => {
    const at = new Date("2026-10-12T14:35:00Z");
    const words = skippedWords(mirror(at, { skipped: "silent" }), new Date("2026-10-12T15:38:49Z"));
    expect(words).toBe("web-2 is not answering. Ployz retries every 15 minutes. Until it catches up, a failover loses 1 hour of writes.");
    expect(words).not.toMatch(/\d\d:\d\d/);
  });

  it("says nothing while rounds land, the mirror is fresh, or a round runs", () => {
    expect(skippedWords(mirror(minutesAgo(135)), now)).toBeNull();
    expect(skippedWords(mirror(minutesAgo(20), { skipped: "silent" }), now)).toBeNull();
    expect(skippedWords(mirror(minutesAgo(135), { skipped: "silent", busy: "Refreshing" }), now)).toBeNull();
    expect(skippedWords(mirror(null, { skipped: "silent" }), now)).toBeNull();
  });
});
