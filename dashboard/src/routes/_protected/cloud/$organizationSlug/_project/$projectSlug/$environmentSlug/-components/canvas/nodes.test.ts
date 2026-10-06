import { describe, expect, it } from "vitest";
import type { ConfigListing, DiffView, RowId, ServiceListing, VolumeListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { configTrays } from "#/modules/config-store/store-configs";
import type { CanvasPosition } from "#/modules/canvas/canvas-positions";
import { buildStoreEdges, buildStoreNodes, canvasNodeOf, shownNodeIds, volumeTrays } from "./nodes";

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
  // SAFETY: test ids stand in for the Store's minted Service ids.
  const listing = (id: string, name: string): ServiceListing => ({ id: id, row: `${id}:node` as RowId, name, private_dns: name, source: "image", change: null, template: null });
  const volume = (id: string, mounts: { service: string; path: string }[]): VolumeListing =>
    ({ id, name: id, mounts, deployed: false, storage: { kind: "docker" }, storage_locked: false, shared_writes: false, change: "create" });
  const listings = [listing("s1", "postgres"), listing("s2", "web")];
  const volumes = [
    volume("shared", [{ service: "postgres", path: "/data" }, { service: "web", path: "/srv" }]),
    volume("own", [{ service: "postgres", path: "/backup" }]),
    volume("loose", []),
    volume("orphan", [{ service: "gone", path: "/data" }]),
  ];
  // The next Deploy mounts `shared` into web; postgres's mount of it stays.
  const diff = asTestDouble<DiffView>()({ changes: [{ type: "service", id: "s2", name: "web", lifecycle: "update", restarts: [], comparison: "head", data: null,
    settings: [{ path: "web.mounts.shared", kind: "add", before: null, after: "/srv", canRestore: true }] }] });
  const { trays, unmounted } = volumeTrays(listings, volumes, diff);
  const config = (id: string, mounts: { service: string; dir: string }[]): ConfigListing =>
    ({ id, name: id, files: [], mounts, deployed: true, change: null });
  const configs = configTrays(listings, [config("sentry", [{ service: "web", dir: "/etc/sentry" }]), config("spare", [])], diff);
  const store = {
    services: listings.map((service) => ({ service, domains: [], changeCount: 0, runtimeIdentity: null, desiredReplicas: null,
      trays: trays.get(service.id) ?? [], configTrays: configs.trays.get(service.id) ?? [] })),
    unmountedVolumes: unmounted,
    unmountedConfigs: configs.unmounted,
  };

  it("puts a mounted Volume in a tray under each Service that mounts it, naming the others", () => {
    expect(trays.get("s1")?.map((tray) => [tray.volume.id, tray.sharedWith])).toEqual([["shared", ["web"]], ["own", []]]);
    expect(trays.get("s2")?.map((tray) => [tray.volume.id, tray.sharedWith])).toEqual([["shared", ["postgres"]]]);
    expect(unmounted.map((listing) => listing.id)).toEqual(["loose", "orphan"]);
  });

  it("marks a tray whose mount the next Deploy stages, under that Service only", () => {
    expect(trays.get("s2")?.map((tray) => [tray.volume.id, tray.mountChanged])).toEqual([["shared", true]]);
    expect(trays.get("s1")?.map((tray) => [tray.volume.id, tray.mountChanged])).toEqual([["shared", false], ["own", false]]);
  });

  it("draws only a Volume or Config nothing here mounts as a node, and grows each Service by its trays", () => {
    const positions = [createCanvasPosition({ resourceId: "s1", x: 480, y: 96 })];
    const nodes = buildStoreNodes(store, positions, "e");
    expect(nodes.map((node) => [node.id, node.type])).toEqual([
      ["s1", "storeService"], ["s2", "storeService"], ["loose", "storeVolume"], ["orphan", "storeVolume"], ["spare", "storeConfig"],
    ]);
    expect(nodes[0]).toMatchObject({ position: { x: 480, y: 96 }, height: 144 + 2 * 40 });
    // web: one Volume tray and one Config tray.
    expect(nodes[1]?.height).toBe(144 + 2 * 40);
    expect(nodes[2]?.data).toMatchObject({ resourceType: "volume", resourceId: "loose", environmentId: "e" });
    expect(nodes[4]?.data).toMatchObject({ resourceType: "config", resourceId: "spare", environmentId: "e" });
  });

  it("places a new node clear of each node's whole height, trays included", () => {
    const tall = createCanvasPosition({ resourceId: "s1", x: 0, y: 0 });
    const nodes = buildStoreNodes(store, [tall], "e");
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
    const nodes = buildStoreNodes(store, [], "e");
    expect(canvasNodeOf(nodes, "shared")).toBe("s1");
    expect(canvasNodeOf(nodes, "sentry")).toBe("s2");
    expect(canvasNodeOf(nodes, "s2")).toBe("s2");
    expect(canvasNodeOf(nodes, "loose")).toBe("loose");
    expect(canvasNodeOf(nodes, "unknown")).toBeNull();
  });

  it("shows what a Deployment Page lights by the nodes that show it, each once", () => {
    expect(shownNodeIds(buildStoreNodes(store, [], "e"), ["shared", "own", "s2", "loose", "gone"])).toEqual(["s1", "s2", "loose"]);
  });
});
