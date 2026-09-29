import type { DiffView } from "@ployz/sdk";
import { describe, expect, it } from "vitest";
import { detachedMounts, mountChange, mountPathError, volumeLoss } from "./store-volumes";

describe("store volumes", () => {
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
      version: "9:1:0.1", saved: 1, published: false, total_count: 2,
      changes: [
        { type: "service", id: "s", name: "postgres", lifecycle: "update", comparison: "head", data: "kept", settings: [
          { path: "postgres.mounts.pg-data", before: "/var/lib/postgresql/data", after: null, kind: "remove", canRestore: false },
          { path: "postgres.replicas", before: 1, after: 2, kind: "update", canRestore: false },
        ] },
        { type: "service", id: "w", name: "web", lifecycle: "update", comparison: "head", data: null, settings: [
          { path: "web.mounts.pg-data", before: null, after: "/srv", kind: "add", canRestore: false },
        ] },
      ],
    };
    expect(detachedMounts(diff, "pg-data")).toEqual([{ service: "postgres", path: "/var/lib/postgresql/data" }]);
    expect(detachedMounts(diff, "other")).toEqual([]);
  });
});
