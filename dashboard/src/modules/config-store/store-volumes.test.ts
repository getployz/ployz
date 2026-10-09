import type { DiffView, RowId } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { detachedMounts, mountChange, mountPathError, gigabytes, volumeLoss, volumeStorage } from "./store-volumes";

describe("store volumes", () => {
  it("uses an exact managed limit and only opts out when explicitly unchecked", () => {
    expect(volumeStorage(true, "5")).toEqual({ kind: "provisioned", maximumBytes: 5_000_000_000 });
    expect(volumeStorage(true, "0.5")).toEqual({ kind: "provisioned", maximumBytes: 500_000_000 });
    expect(volumeStorage(true, ".001")).toEqual({ kind: "provisioned", maximumBytes: 1_000_000 });
    expect(volumeStorage(true, "1.073741824")).toEqual({ kind: "provisioned", maximumBytes: 1_073_741_824 });
    expect(volumeStorage(true, gigabytes(Number.MAX_SAFE_INTEGER))).toEqual({ kind: "provisioned", maximumBytes: Number.MAX_SAFE_INTEGER });
    // Decimal GB read exactly: floating point would make these inexact and refuse them.
    expect(volumeStorage(true, "1.001")).toEqual({ kind: "provisioned", maximumBytes: 1_001_000_000 });
    expect(volumeStorage(true, "4.1")).toEqual({ kind: "provisioned", maximumBytes: 4_100_000_000 });
    expect([1_001_000_000, 4_100_000_000, 5_000_000_000, 1_073_741_824].map(gigabytes)).toEqual(["1.001", "4.1", "5", "1.073741824"]);
    for (const invalid of ["", ".", "0", "-1", "bad", "Infinity", "0.000999999", "1.0000000001", "9007199.254740992", "9007199254.740992"]) {
      expect(volumeStorage(true, invalid)).toBeNull();
    }
    expect(volumeStorage(false, "")).toEqual({ kind: "docker" });
  });
  it("mounts at a path and detaches by unsetting the mount", () => {
    expect(mountChange("web", "data", "/srv")).toEqual({ op: "set", path: "web.mounts.data", value: "/srv" });
    expect(mountChange("web", "data", null)).toEqual({ op: "unset", path: "web.mounts.data" });
    expect(mountPathError("srv")).toMatch(/absolute/u);
    expect(mountPathError("/srv")).toBeNull();
  });

  it("reads what a refused Deploy would delete, and nothing from any other refusal", () => {
    const details = {
      volumes: [{ id: "v", name: "pg-data", docker_volume: "ns_vol-v", deletes: [{ machine_id: "m1", name: "ns_vol-v" }] }],
      accept: ["pg-data"],
      version: "9:1:0.1",
    };
    expect(volumeLoss({ code: "confirmation_required", details })).toEqual({
      volumes: [{ name: "pg-data", deletes: [{ machine_id: "m1" }] }], accept: ["pg-data"], version: "9:1:0.1",
    });
    // Servers that weren't checked: the Deploy fails closed, with nothing to accept.
    expect(volumeLoss({ code: "unavailable", details: { volumes: ["pg-data"] } })).toBeNull();
    expect(volumeLoss({ code: "confirmation_required", details: { accept: [] } })).toBeNull();
  });

  it("lists mounts the next Deploy detaches, keeping their data", () => {
    const diff: DiffView = {
      environment: { id: "e", project: "shop", name: "production", revision: 9 },
      version: "9:1:0.1", saved: 1, published: false, total_count: 2, hints: [], incoming: [], follow_hints: [],
      changes: [
        { type: "service", id: "s", row: "s:node" as RowId, name: "postgres", lifecycle: "update", comparison: "head", data: "kept", restarts: [], settings: [
          { path: "postgres.mounts.pg-data", before: "/var/lib/postgresql/data", after: null, kind: "remove", canRestore: false, row: null },
          { path: "postgres.replicas", before: 1, after: 2, kind: "update", canRestore: false, row: null },
        ] },
        { type: "service", id: "w", row: "w:node" as RowId, name: "web", lifecycle: "update", comparison: "head", data: null, restarts: [], settings: [
          { path: "web.mounts.pg-data", before: null, after: "/srv", kind: "add", canRestore: false, row: null },
        ] },
      ],
    };
    expect(detachedMounts(diff, "pg-data")).toEqual([{ service: "postgres", path: "/var/lib/postgresql/data" }]);
    expect(detachedMounts(diff, "other")).toEqual([]);
  });
});
