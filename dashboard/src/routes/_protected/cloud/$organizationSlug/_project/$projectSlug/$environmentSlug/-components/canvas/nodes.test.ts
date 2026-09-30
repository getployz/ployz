import { describe, expect, it } from "vitest";
import type { ServiceListing, VolumeListing } from "@ployz/sdk";
import type { CanvasPosition } from "#/modules/canvas/canvas-positions";
import { buildStoreEdges, buildStoreNodes, canvasNodeOf, volumeTrays } from "./nodes";

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
    const listing: ServiceListing = { id: id, name, private_dns: name, source: "image", change: null, template: null };
    return { service: listing, domains: [], changeCount: 0, runtimeIdentity: null, uploaded: false, desiredReplicas: null };
  };
  const volume = (id: string, mounts: { service: string; path: string }[]): VolumeListing =>
    ({ id, name: id, mounts, deployed: false, storage: { kind: "docker" }, storage_locked: false, change: "create" });
  const store = {
    services: [service("s1", "postgres"), service("s2", "web")],
    volumes: [
      volume("shared", [{ service: "postgres", path: "/data" }, { service: "web", path: "/srv" }]),
      volume("own", [{ service: "postgres", path: "/backup" }]),
      volume("loose", []),
      volume("orphan", [{ service: "gone", path: "/data" }]),
    ],
  };

  it("puts a mounted Volume in a tray under each Service that mounts it, naming the others", () => {
    const { trays, unmounted } = volumeTrays(store.services, store.volumes);
    expect(trays.get("s1")?.map((tray) => [tray.volume.id, tray.sharedWith])).toEqual([["shared", ["web"]], ["own", []]]);
    expect(trays.get("s2")?.map((tray) => [tray.volume.id, tray.sharedWith])).toEqual([["shared", ["postgres"]]]);
    expect(unmounted.map((listing) => listing.id)).toEqual(["loose", "orphan"]);
  });

  it("draws only a Volume nothing here mounts as a node, and grows each Service by its trays", () => {
    const positions = [createCanvasPosition({ resourceId: "s1", x: 480, y: 96 })];
    const nodes = buildStoreNodes(store, positions, "loose", "e");
    expect(nodes.map((node) => [node.id, node.type, node.selected])).toEqual([
      ["s1", "storeService", false], ["s2", "storeService", false], ["loose", "storeVolume", true], ["orphan", "storeVolume", false],
    ]);
    expect(nodes[0]).toMatchObject({ position: { x: 480, y: 96 }, height: 144 + 2 * 40 });
    expect(nodes[1]?.height).toBe(144 + 40);
    expect(nodes[2]?.data).toMatchObject({ resourceType: "volume", resourceId: "loose", environmentId: "e" });
  });

  it("places a new node clear of each node's whole height, trays included", () => {
    const tall = createCanvasPosition({ resourceId: "s1", x: 0, y: 0 });
    const nodes = buildStoreNodes(store, [tall], null, "e");
    const web = nodes.find((node) => node.id === "s2");
    const clear = (web?.position.y ?? 0) >= 144 + 2 * 40 || (web?.position.x ?? 0) >= 288;
    expect(clear).toBe(true);
  });

  it("links only Live Nodes: a mount is a tray, not a link", () => {
    expect(buildStoreEdges({})).toEqual([]);
    expect(buildStoreEdges({ live: [{ name: "redis", owner: "production", data: false, usedBy: ["s2"] }] }))
      .toMatchObject([{ id: "live:redis:s2", source: "live:redis", target: "s2" }]);
  });

  it("finds the node that shows a selection: a tray's first Service, else the node itself", () => {
    const nodes = buildStoreNodes(store, [], null, "e");
    expect(canvasNodeOf(nodes, "shared")).toBe("s1");
    expect(canvasNodeOf(nodes, "s2")).toBe("s2");
    expect(canvasNodeOf(nodes, "loose")).toBe("loose");
    expect(canvasNodeOf(nodes, "unknown")).toBeNull();
  });
});
