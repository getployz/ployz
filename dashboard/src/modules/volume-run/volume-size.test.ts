import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { PoolShortfall } from "#/modules/volume-run/resize";
import { recordRefusalOf, sizeRefusalOf } from "#/modules/volume-run/volume-size.server";

const GB = 1_000_000_000;
const db1 = { id: "m-1".padEnd(32, "0") as MachineId, name: "db-1" };
const refused = (details: { reason: string; bound_bytes?: number; capacity?: PoolShortfall; floor_bytes?: number }) => ({ code: "refused", message: "refused", details });
const lacking: PoolShortfall = { code: "insufficient_storage", required_growth_bytes: 4 * GB };
const stuck: PoolShortfall = { code: "pool_cannot_grow" };

describe("sizeRefusalOf in a grow", () => {
  it.each<[string, unknown, unknown]>([
    ["a transport failure carries no refusal", undefined, null],
    ["a refusal without a reason is no refusal", { code: "x", details: {} }, null],
    ["a copy already bigger holds", refused({ reason: "shrink", bound_bytes: 30 * GB }), { kind: "holds", boundBytes: 30 * GB }],
    ["a copy at the size holds", refused({ reason: "shrink", bound_bytes: 20 * GB }), { kind: "holds", boundBytes: 20 * GB }],
    ["a shrink refusal below the size is final", refused({ reason: "shrink", bound_bytes: 15 * GB }), { kind: "refused", reason: "shrink", boundBytes: 15 * GB, capacity: null, floorBytes: null, message: "db-1 refused to resize data (shrink)" }],
    ["a switching copy retries", refused({ reason: "switching" }), { kind: "retry" }],
    [
      "no capacity names the Server and the size, not the Pool's growth",
      refused({ reason: "no_capacity", bound_bytes: 10 * GB, capacity: lacking }),
      { kind: "refused", reason: "no_capacity", boundBytes: 10 * GB, capacity: lacking, floorBytes: null, message: "db-1 has no room to grow data to 20 GB" },
    ],
    [
      "no capacity on a Pool that cannot grow names only the Server",
      refused({ reason: "no_capacity", capacity: stuck }),
      { kind: "refused", reason: "no_capacity", boundBytes: null, capacity: stuck, floorBytes: null, message: "db-1 has no room to grow data to 20 GB" },
    ],
    ["a copy gone from the Server", refused({ reason: "not_found" }), { kind: "refused", reason: "not_found", boundBytes: null, capacity: null, floorBytes: null, message: "db-1 no longer holds a copy of data" }],
    ["a copy without a bound", refused({ reason: "not_provisioned" }), { kind: "refused", reason: "not_provisioned", boundBytes: null, capacity: null, floorBytes: null, message: "data on db-1 has no size limit to change" }],
    ["a reason this Cloud doesn't know is final", refused({ reason: "too_hot", bound_bytes: 10 * GB }), { kind: "refused", reason: "too_hot", boundBytes: 10 * GB, capacity: null, floorBytes: null, message: "db-1 refused to resize data (too_hot)" }],
  ])("%s", (_, cause, expected) => {
    const outcome = sizeRefusalOf("data", db1, 20 * GB, "grow", cause);
    if (expected === null) expect(outcome).toBeNull();
    else expect(outcome).toEqual(expected);
  });
});

describe("sizeRefusalOf in a settle", () => {
  it.each<[string, unknown, unknown]>([
    [
      "a copy above the size whose daemon ignored the shrink needs a newer Ployz",
      refused({ reason: "shrink", bound_bytes: 20 * GB }),
      { kind: "refused", reason: "shrink", boundBytes: 20 * GB, capacity: null, floorBytes: null, message: "db-1 needs a newer Ployz to shrink data" },
    ],
    [
      "a copy below its floor names the floor rounded up to a whole GB",
      refused({ reason: "below_floor", bound_bytes: 20 * GB, floor_bytes: 8_030_000_000 }),
      { kind: "refused", reason: "below_floor", boundBytes: 20 * GB, capacity: null, floorBytes: 8_030_000_000, message: "db-1 can't shrink data to 5 GB; its data needs at least 9 GB" },
    ],
  ])("%s", (_, cause, expected) => {
    expect(sizeRefusalOf("data", db1, 5 * GB, "settle", cause)).toEqual(expected);
  });
});

describe("sizeRefusalOf in a shrink", () => {
  it.each<[string, unknown, unknown]>([
    [
      "a copy that refuses as a shrink ignored the ask, so it needs a newer Ployz",
      refused({ reason: "shrink", bound_bytes: 20 * GB }),
      { kind: "refused", reason: "shrink", boundBytes: 20 * GB, capacity: null, floorBytes: null, message: "db-1 needs a newer Ployz to shrink data" },
    ],
    [
      "a copy below its floor names the floor",
      refused({ reason: "below_floor", bound_bytes: 20 * GB, floor_bytes: 12 * GB }),
      { kind: "refused", reason: "below_floor", boundBytes: 20 * GB, capacity: null, floorBytes: 12 * GB, message: "db-1 can't shrink data to 10 GB; its data needs at least 12 GB" },
    ],
    ["a switching copy retries", refused({ reason: "switching" }), { kind: "retry" }],
    [
      "a mirror below the size is grown to it, so no capacity reads as a grow",
      refused({ reason: "no_capacity", bound_bytes: 5 * GB, capacity: lacking }),
      { kind: "refused", reason: "no_capacity", boundBytes: 5 * GB, capacity: lacking, floorBytes: null, message: "db-1 has no room to grow data to 10 GB" },
    ],
    ["a reason this Cloud doesn't know is final", refused({ reason: "too_hot" }), { kind: "refused", reason: "too_hot", boundBytes: null, capacity: null, floorBytes: null, message: "db-1 refused to resize data (too_hot)" }],
  ])("%s", (_, cause, expected) => {
    expect(sizeRefusalOf("data", db1, 10 * GB, "shrink", cause)).toEqual(expected);
  });
});

describe("recordRefusalOf", () => {
  const shrink = { from: 20 * GB, to: 10 * GB };
  it.each<[string, { code: string; details: unknown }, unknown]>([
    [
      "a shrink refused while a Deployment is in flight names it and asks again when it ends",
      { code: "conflict", details: { reason: "deploying", volume: "v", deployment: { id: "d", number: 7 } } },
      { ok: false, reason: "changed", message: "Deployment #7 is in flight; run ployz volume set data --size 10 again when it ends" },
    ],
    [
      "an unnumbered Deployment is named as one",
      { code: "conflict", details: { reason: "deploying", volume: "v", deployment: { id: "d", number: null } } },
      { ok: false, reason: "changed", message: "A Deployment is in flight; run ployz volume set data --size 10 again when it ends" },
    ],
    [
      "a size recorded since names both",
      { code: "conflict", details: { reason: "size_changed", volume: "v", recorded: { kind: "provisioned", maximumBytes: 30 * GB } } },
      { ok: false, reason: "changed", message: "the Store records data at 30 GB, not 20 GB; read its size again" },
    ],
    ["a Volume no longer deployed is gone", { code: "not_found", details: null }, { ok: false, reason: "gone", message: "data is no longer deployed, so its size was not recorded" }],
    ["a Store that couldn't answer ends nothing", { code: "unavailable", details: null }, null],
  ])("%s", (_, refusal, expected) => {
    expect(recordRefusalOf("data", shrink, refusal as { code: string; details: null })).toEqual(expected);
  });
});
