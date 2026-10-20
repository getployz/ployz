import { describe, expect, it } from "vitest";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { checkResize, growWords, type Holder, holdersOf, type ResizeAsk, type ResizeVerdict } from "#/modules/volume-run/resize-check";
import { member } from "#/modules/volume-run/volume-run.test-fixture";

const GB = 1_000_000_000;
const SIZE = ["ployz.volume.size.set.v1"];
const holder = (name: string, role: Holder["role"], options: Partial<Holder> = {}): Holder =>
  ({ name, role, capabilities: SIZE, committableBytes: null, ...options });
const ask = (requested: number, options: Partial<ResizeAsk> = {}): ResizeAsk =>
  ({ volume: "data", recorded: 10 * GB, working: 10 * GB, requested, holders: [holder("fsn-1", "primary"), holder("fsn-2", "mirror")], ...options });

describe("checkResize", () => {
  it.each<[string, ResizeAsk, ResizeVerdict]>([
    ["a smaller size is refused, as the Store refuses it", ask(5 * GB), { kind: "refused", message: "data is deployed at 10 GB; a deployed volume can't shrink yet." }],
    ["the held size is settled", ask(10 * GB), { kind: "settled" }],
    ["the held size over a restored smaller revision stages it back", ask(10 * GB, { working: 4 * GB }), { kind: "restage" }],
    ["a larger size grows the primary and its mirrors", ask(20 * GB), { kind: "grow", from: 10 * GB, to: 20 * GB, servers: ["fsn-1", "fsn-2"] }],
    ["before the Servers answer, a larger size grows without naming them", ask(20 * GB, { holders: null }), { kind: "grow", from: 10 * GB, to: 20 * GB, servers: null }],
    [
      "every holder without the verb is named, an old copy and a record listing nothing included",
      ask(20 * GB, { holders: [holder("fsn-1", "primary", { capabilities: [] }), holder("fsn-2", "mirror"), holder("fsn-3", "old_copy", { capabilities: ["ployz.volume.copy.set.v1"] })] }),
      { kind: "refused", message: "Growing needs a newer Ployz on fsn-1, fsn-3." },
    ],
    [
      "a holder Cloud has no record of may not serve the verb",
      ask(20 * GB, { holders: [holder("fsn-1", "primary", { capabilities: null })] }),
      { kind: "refused", message: "Growing needs a newer Ployz on fsn-1." },
    ],
    [
      "the tightest Pool among primary and mirrors refuses, its room rounded down",
      ask(25 * GB, { holders: [holder("fsn-1", "primary", { committableBytes: 40 * GB }), holder("fsn-2", "mirror", { committableBytes: 12_884_901_888 })] }),
      { kind: "refused", message: "fsn-2 has room to grow it by 12 GB at most." },
    ],
    [
      "a Pool with less than a gigabyte has no room",
      ask(11 * GB, { holders: [holder("fsn-1", "primary", { committableBytes: 0.5 * GB })] }),
      { kind: "refused", message: "fsn-1 has no room to grow it." },
    ],
    [
      "growth that fits exactly is let through",
      ask(22 * GB, { holders: [holder("fsn-1", "primary", { committableBytes: 12 * GB })] }),
      { kind: "grow", from: 10 * GB, to: 22 * GB, servers: ["fsn-1"] },
    ],
    [
      "an unknown room refuses nothing, and an old copy's room isn't read",
      ask(500 * GB, { holders: [holder("fsn-1", "primary"), holder("fsn-3", "old_copy", { committableBytes: 0 })] }),
      { kind: "grow", from: 10 * GB, to: 500 * GB, servers: ["fsn-1"] },
    ],
    [
      "the Servers are named primary first",
      ask(20 * GB, { holders: [holder("fsn-2", "mirror"), holder("fsn-3", "old_copy"), holder("fsn-1", "primary")] }),
      { kind: "grow", from: 10 * GB, to: 20 * GB, servers: ["fsn-1", "fsn-2"] },
    ],
  ])("%s", (_, input, verdict) => {
    expect(checkResize(input)).toEqual(verdict);
  });
});

describe("growWords", () => {
  it.each<[string, readonly string[] | null, string]>([
    ["names the primary and its mirror", ["fsn-1", "fsn-2"], "Grow data from 10 GB to 20 GB on fsn-1 and its mirror on fsn-2. Its services keep running."],
    ["names every mirror", ["fsn-1", "fsn-2", "fsn-3"], "Grow data from 10 GB to 20 GB on fsn-1 and its mirrors on fsn-2 and fsn-3. Its services keep running."],
    ["names a lone primary", ["fsn-1"], "Grow data from 10 GB to 20 GB on fsn-1. Its services keep running."],
    ["says every copy before the Servers answer", null, "Grow data from 10 GB to 20 GB on every server that holds a copy. Its services keep running."],
  ])("%s", (_, servers, words) => {
    expect(growWords("data", { kind: "grow", from: 10 * GB, to: 20 * GB, servers })).toBe(words);
  });
});

describe("holdersOf", () => {
  const record = (id: string, capabilities: readonly string[], poolCommittableBytes: number | null) =>
    ({ id: id.padEnd(32, "0"), capabilities, poolCommittableBytes }) as RuntimeMachineRecord;

  it("reads each copy's Server record, and none for a Server without one", () => {
    const members = [member("fsn-2", "mirror"), member("fsn-1", "writer"), member("fsn-3", "old"), member("fsn-4", "empty")];
    expect(holdersOf(members, [record("fsn-1", SIZE, 30 * GB), record("fsn-2", [], null)])).toEqual([
      { name: "fsn-1", role: "primary", capabilities: SIZE, committableBytes: 30 * GB },
      { name: "fsn-2", role: "mirror", capabilities: [], committableBytes: null },
      { name: "fsn-3", role: "old_copy", capabilities: null, committableBytes: null },
    ]);
  });

  it("knows no holders until the Servers answer and their records are in", () => {
    expect(holdersOf(undefined, [])).toBeNull();
    expect(holdersOf([member("fsn-1", "writer")], null)).toBeNull();
  });
});
