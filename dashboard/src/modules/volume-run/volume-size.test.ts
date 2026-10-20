import { describe, expect, it } from "vitest";
import type { MachineId } from "@ployz/sdk";
import type { PoolShortfall } from "#/modules/volume-run/resize";
import { sizeRefusalOf } from "#/modules/volume-run/volume-size.server";

const GB = 1_000_000_000;
const db1 = { id: "m-1".padEnd(32, "0") as MachineId, name: "db-1" };
const refused = (details: { reason: string; bound_bytes?: number; capacity?: PoolShortfall }) => ({ code: "refused", message: "refused", details });
const lacking: PoolShortfall = { code: "insufficient_storage", required_growth_bytes: 4 * GB };
const stuck: PoolShortfall = { code: "pool_cannot_grow" };

describe("sizeRefusalOf", () => {
  it.each<[string, unknown, unknown]>([
    ["a transport failure carries no refusal", undefined, null],
    ["a refusal without a reason is no refusal", { code: "x", details: {} }, null],
    ["a copy already bigger holds", refused({ reason: "shrink", bound_bytes: 30 * GB }), { kind: "holds", boundBytes: 30 * GB }],
    ["a copy at the size holds", refused({ reason: "shrink", bound_bytes: 20 * GB }), { kind: "holds", boundBytes: 20 * GB }],
    ["a shrink refusal below the size is final", refused({ reason: "shrink", bound_bytes: 15 * GB }), { kind: "refused", reason: "shrink", boundBytes: 15 * GB, capacity: null, message: "db-1 refused to resize data (shrink)" }],
    ["a switching copy retries", refused({ reason: "switching" }), { kind: "retry" }],
    [
      "no capacity names the Server and the size, not the Pool's growth",
      refused({ reason: "no_capacity", bound_bytes: 10 * GB, capacity: lacking }),
      { kind: "refused", reason: "no_capacity", boundBytes: 10 * GB, capacity: lacking, message: "db-1 has no room to grow data to 20 GB" },
    ],
    [
      "no capacity on a Pool that cannot grow names only the Server",
      refused({ reason: "no_capacity", capacity: stuck }),
      { kind: "refused", reason: "no_capacity", boundBytes: null, capacity: stuck, message: "db-1 has no room to grow data to 20 GB" },
    ],
    ["a copy gone from the Server", refused({ reason: "not_found" }), { kind: "refused", reason: "not_found", boundBytes: null, capacity: null, message: "db-1 no longer holds a copy of data" }],
    ["a copy without a bound", refused({ reason: "not_provisioned" }), { kind: "refused", reason: "not_provisioned", boundBytes: null, capacity: null, message: "data on db-1 has no size limit to change" }],
    ["a reason this Cloud doesn't know is final", refused({ reason: "below_floor", bound_bytes: 10 * GB }), { kind: "refused", reason: "below_floor", boundBytes: 10 * GB, capacity: null, message: "db-1 refused to resize data (below_floor)" }],
  ])("%s", (_, cause, expected) => {
    const outcome = sizeRefusalOf("data", db1, 20 * GB, cause);
    if (expected === null) expect(outcome).toBeNull();
    else expect(outcome).toEqual(expected);
  });
});
