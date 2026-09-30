import { describe, expect, it } from "vitest";
import type { ServiceListing, VolumeListing } from "@ployz/sdk";
import type { CanvasPosition } from "#/modules/canvas/canvas-positions";
import { buildStoreEdges, buildStoreNodes } from "./nodes";

function createCanvasPosition(overrides?: Partial<CanvasPosition>): CanvasPosition {
  return {
    id: "pos-1",
    organizationId: "org-1",
    environmentId: "env-1",
    resourceType: "service",
    resourceId: "service-1",
    x: 400,
    y: 280,
    createdAt: new Date("2026-03-26T00:00:00.000Z"),
    updatedAt: new Date("2026-03-26T00:00:00.000Z"),
    ...overrides,
  };
}

describe("Config Store nodes", () => {
  const service = (id: string, name: string) => {
    // SAFETY: test ids stand in for the Store's minted Service ids.
    const listing: ServiceListing = { id: id, name, private_dns: name, source: "image", change: null };
    return { service: listing, subtitle: null, changeCount: 0, runtimeIdentity: null, uploaded: false };
  };
  const volume = (id: string, mounts: { service: string; path: string }[]): VolumeListing =>
    ({ id, name: id, mounts, deployed: false, storage: { kind: "docker" }, storage_locked: false, change: "create" });

  it("links each Volume into the Services that mount it", () => {
    const store = { services: [service("s1", "postgres"), service("s2", "web")], volumes: [volume("v1", [{ service: "postgres", path: "/data" }, { service: "web", path: "/srv" }])] };
    expect(buildStoreEdges(store)).toEqual([
      { id: "mount:v1:s1", source: "v1", target: "s1" },
      { id: "mount:v1:s2", source: "v1", target: "s2" },
    ]);
  });

  it("keeps stored positions and puts an unplaced Volume below the Service that mounts it", () => {
    const store = { services: [service("s1", "postgres")], volumes: [volume("v1", [{ service: "postgres", path: "/data" }]), volume("v2", [])] };
    const positions = [createCanvasPosition({ resourceId: "s1", x: 480, y: 96 })];
    const nodes = buildStoreNodes(store, positions, "v1", "e");
    expect(nodes.map((node) => [node.id, node.type, node.selected])).toEqual([["s1", "storeService", false], ["v1", "storeVolume", true], ["v2", "storeVolume", false]]);
    expect(nodes[0]?.position).toEqual({ x: 480, y: 96 });
    expect(nodes[1]?.position.y).toBeGreaterThan(96 + 144);
    expect(nodes[1]?.data).toMatchObject({ resourceType: "volume", resourceId: "v1", environmentId: "e" });
  });
});
