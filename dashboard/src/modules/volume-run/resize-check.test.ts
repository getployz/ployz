import type { MachineId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import type { RuntimeMachineRecord, RuntimeVolumeCopy } from "#/modules/runtime/runtime.collection";
import { checkResize, type Holder, holdersOf, type ResizeAsk, type ResizeVerdict } from "#/modules/volume-run/resize-check";
import { member } from "#/modules/volume-run/volume-run.test-fixture";

const GB = 1_000_000_000;
const SIZE = ["ployz.volume.size.set.v1"];
const holder = (name: string, role: Holder["role"], options: Partial<Holder> = {}): Holder =>
  ({ id: name.padEnd(32, "0") as MachineId, name, role, capabilities: SIZE, committableBytes: null, boundBytes: null, passkey: false, ...options });
const ask = (requested: number, options: Partial<ResizeAsk> = {}): ResizeAsk =>
  ({ volume: "data", recorded: 10 * GB, working: 10 * GB, requested, holders: [holder("fsn-1", "primary"), holder("fsn-2", "mirror")], ...options });

describe("checkResize", () => {
  it.each<[string, ResizeAsk, ResizeVerdict]>([
    ["a smaller size is refused, as the Store refuses it", ask(5 * GB), { kind: "refused", message: "data is deployed at 10 GB; this panel can't shrink it. Use ployz volume set data --size 5." }],
    ["the held size is settled", ask(10 * GB), { kind: "settled" }],
    ["the held size over a restored smaller revision stages it back", ask(10 * GB, { working: 4 * GB }), { kind: "restage" }],
    ["a larger size grows the primary and its mirrors", ask(20 * GB), { kind: "grow", from: 10 * GB, to: 20 * GB, copies: [{ name: "fsn-1", role: "primary" }, { name: "fsn-2", role: "mirror" }] }],
    ["before the Servers answer, a larger size grows without naming them", ask(20 * GB, { holders: null }), { kind: "grow", from: 10 * GB, to: 20 * GB, copies: null }],
    [
      "every holder without the verb is named, an old copy and a record listing nothing included",
      ask(20 * GB, { holders: [holder("fsn-1", "primary", { capabilities: [] }), holder("fsn-2", "mirror"), holder("fsn-3", "old_copy", { capabilities: ["ployz.volume.copy.set.v1"] })] }),
      { kind: "refused", message: "Growing needs a newer Ployz on fsn-1, fsn-3." },
    ],
    [
      "a holder Cloud has no record of can't be checked",
      ask(20 * GB, { holders: [holder("fsn-1", "primary", { capabilities: null }), holder("fsn-2", "mirror")] }),
      { kind: "refused", message: "fsn-1 isn't reporting to Cloud, so growing can't check it." },
    ],
    [
      "copies with no primary have no deployed size to grow, as the run refuses",
      ask(20 * GB, { holders: [holder("fsn-2", "mirror"), holder("fsn-3", "old_copy")] }),
      { kind: "refused", message: "No server answers with data's primary copy, so it can't grow now." },
    ],
    [
      "no copy anywhere has no deployed size to grow",
      ask(20 * GB, { holders: [] }),
      { kind: "refused", message: "No server answers with data's primary copy, so it can't grow now." },
    ],
    [
      "a retry after a partial grow needs no room on the copy that already grew",
      ask(20 * GB, { holders: [holder("fsn-1", "primary", { committableBytes: 15 * GB, boundBytes: 10 * GB }), holder("fsn-2", "mirror", { committableBytes: 0, boundBytes: 20 * GB })] }),
      { kind: "grow", from: 10 * GB, to: 20 * GB, copies: [{ name: "fsn-1", role: "primary" }, { name: "fsn-2", role: "mirror" }] },
    ],
    [
      "a copy's own bound sets the room it needs",
      ask(20 * GB, { holders: [holder("fsn-1", "primary"), holder("fsn-2", "mirror", { committableBytes: 4 * GB, boundBytes: 15 * GB })] }),
      { kind: "refused", message: "fsn-2 has room to grow it by 4 GB at most." },
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
      { kind: "grow", from: 10 * GB, to: 22 * GB, copies: [{ name: "fsn-1", role: "primary" }] },
    ],
    [
      "an unknown room refuses nothing",
      ask(500 * GB, { holders: [holder("fsn-1", "primary")] }),
      { kind: "grow", from: 10 * GB, to: 500 * GB, copies: [{ name: "fsn-1", role: "primary" }] },
    ],
    [
      "an old copy grows too, so its Pool's room refuses",
      ask(20 * GB, { holders: [holder("fsn-1", "primary"), holder("fsn-3", "old_copy", { committableBytes: 0 })] }),
      { kind: "refused", message: "fsn-3 has no room to grow it." },
    ],
    [
      "every copy is named, primary first, then mirrors and old copies",
      ask(20 * GB, { holders: [holder("fsn-3", "old_copy"), holder("fsn-2", "mirror"), holder("fsn-1", "primary")] }),
      { kind: "grow", from: 10 * GB, to: 20 * GB, copies: [{ name: "fsn-1", role: "primary" }, { name: "fsn-2", role: "mirror" }, { name: "fsn-3", role: "old_copy" }] },
    ],
  ])("%s", (_, input, verdict) => {
    expect(checkResize(input)).toEqual(verdict);
  });
});

describe("holdersOf", () => {
  const record = (id: string, capabilities: readonly string[], poolCommittableBytes: number | null) =>
    ({ id: id.padEnd(32, "0"), capabilities, poolCommittableBytes }) as RuntimeMachineRecord;
  const copy = (server: string, volumeId: string, boundBytes: number | null): RuntimeVolumeCopy =>
    ({ machineId: server.padEnd(32, "0"), name: `shop-production_vol-${volumeId}`, role: "slot", slotRole: "mirror", boundBytes });

  it("reads each copy's Server record and its bound there, and none for a Server without them", () => {
    const members = [member("fsn-2", "mirror"), member("fsn-1", "writer"), member("fsn-3", "old"), member("fsn-4", "empty")];
    const copies = [copy("fsn-1", "data", 10 * GB), copy("fsn-2", "data", 20 * GB), copy("fsn-2", "other", 50 * GB), copy("fsn-3", "data", null)];
    expect(holdersOf("data", members, [record("fsn-1", SIZE, 30 * GB), record("fsn-2", [], null)], copies)).toEqual([
      { id: "fsn-1".padEnd(32, "0"), name: "fsn-1", role: "primary", capabilities: SIZE, committableBytes: 30 * GB, boundBytes: 10 * GB, passkey: true },
      { id: "fsn-2".padEnd(32, "0"), name: "fsn-2", role: "mirror", capabilities: [], committableBytes: null, boundBytes: 20 * GB, passkey: true },
      { id: "fsn-3".padEnd(32, "0"), name: "fsn-3", role: "old_copy", capabilities: null, committableBytes: null, boundBytes: null, passkey: true },
    ]);
  });

  it("knows no holders until the Servers answer and their records are in", () => {
    expect(holdersOf("data", undefined, [], [])).toBeNull();
    expect(holdersOf("data", [member("fsn-1", "writer")], null, [])).toBeNull();
  });
});
