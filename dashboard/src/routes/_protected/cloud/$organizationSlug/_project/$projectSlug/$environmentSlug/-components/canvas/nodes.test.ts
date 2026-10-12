import { describe, expect, it } from "vitest";
import type { ConfigListing, DiffView, RowId, ServiceListing, VolumeListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { configTrays } from "#/modules/config-store/store-configs";
import type { CanvasPosition } from "#/modules/canvas/canvas-positions";
import type { MachineId } from "@ployz/sdk";
import type { MirrorCopy, OffsiteCopy, OldCopy, VolumeCopies } from "#/modules/volume-copies/volume-copies";
import { buildStoreEdges, buildStoreNodes, canvasNodeOf, hangOverhang, shownNodeIds, volumeTrays } from "./nodes";

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
      trays: trays.get(service.id) ?? [], configTrays: configs.trays.get(service.id) ?? [], notSet: [] })),
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

  it("finds a mirror by the node that shows its Volume", () => {
    const nodes = buildStoreNodes(store, [], "e");
    expect(canvasNodeOf(nodes, "shared~web-2")).toBe("s1");
    expect(canvasNodeOf(nodes, "loose~web-3")).toBe("loose");
  });

  it("shows what a Deployment Page lights by the nodes that show it, each once", () => {
    expect(shownNodeIds(buildStoreNodes(store, [], "e"), ["shared", "own", "s2", "loose", "gone"])).toEqual(["s1", "s2", "loose"]);
  });

  describe("copies", () => {
    const server = (name: string) => ({ id: name as MachineId, name });
    const mirror = (name: string): MirrorCopy =>
      ({ kind: "mirror", id: name, server: server(name), name, refreshedAt: null, skipped: false, staged: null, busy: null });
    const offsite: OffsiteCopy = { kind: "offsite", id: "b", bucket: "b", region: "r", keep: 7, uploadedAt: null, staged: null, busy: null };
    const old: OldCopy = { kind: "old", id: "web-9", server: server("web-9"), name: "old", since: null, bytes: null, staged: null, busy: null };
    const copiesOf = (id: string, elsewhere: VolumeCopies["elsewhere"]): VolumeCopies => ({
      volume: { id, name: id }, live: { server: server("web-1"), restore: null, moveTo: null, busy: null, snapshots: null }, elsewhere,
    });
    const copies = new Map([
      ["shared", copiesOf("shared", [mirror("web-2")])], ["loose", copiesOf("loose", [mirror("web-3")])], ["own", copiesOf("own", [old])],
    ]);
    const withCopies = { ...store, copies };

    it("hangs a mounted Volume's copies under the first Service that shows it, once, and grows that Service", () => {
      const nodes = buildStoreNodes(withCopies, [], "e");
      const s1 = nodes.find((node) => node.id === "s1");
      const s2 = nodes.find((node) => node.id === "s2");
      expect(s1?.data).toMatchObject({ hang: [{ volume: { id: "shared" } }] });
      expect(s1?.height).toBe(144 + 2 * 40 + 80);
      expect(s2?.data).toMatchObject({ hang: [] });
      expect(s2?.height).toBe(144 + 2 * 40);
    });

    it("hangs an unmounted Volume's copies under its own node", () => {
      const loose = buildStoreNodes(withCopies, [], "e").find((node) => node.id === "loose");
      expect(loose?.data).toMatchObject({ hang: [{ volume: { id: "loose" } }] });
      expect(loose?.height).toBe(144 + 80);
    });

    it("hangs nothing and adds no height for a Volume whose only other copy is old", () => {
      const nodes = buildStoreNodes({ ...store, copies: new Map([["own", copiesOf("own", [old])]]) }, [], "e");
      expect(nodes.find((node) => node.id === "s1")).toMatchObject({ height: 144 + 2 * 40, data: { hang: [] } });
    });

    it("places a new node clear of a Service's copies", () => {
      const nodes = buildStoreNodes(withCopies, [createCanvasPosition({ resourceId: "s1", x: 0, y: 0 })], "e");
      const { x, y } = nodes.find((node) => node.id === "s2")?.position ?? { x: 0, y: 0 };
      const clear = y >= 144 + 2 * 40 + 80 || y + 144 + 2 * 40 <= 0 || x >= 288 || x + 288 <= 0;
      expect(clear).toBe(true);
    });

    it("keeps a stored position as it is when a copy appears", () => {
      const nodes = buildStoreNodes(withCopies, [createCanvasPosition({ resourceId: "s1", x: 480, y: 96 })], "e");
      expect(nodes[0]?.position).toEqual({ x: 480, y: 96 });
    });

    it("gives each tray its Volume's copies, and none to a Volume without", () => {
      const { trays: withTrays } = volumeTrays(listings, volumes, diff, () => 1, copies);
      expect(withTrays.get("s1")?.map((tray) => [tray.volume.id, tray.copies?.elsewhere.length ?? null])).toEqual([["shared", 1], ["own", 1]]);
      expect(volumeTrays(listings, volumes, diff).trays.get("s1")?.map((tray) => tray.copies)).toEqual([null, null]);
    });

    it("measures how far a row of cards reaches past its node on each side", () => {
      expect(hangOverhang([copiesOf("a", [mirror("web-2")])])).toBe(0);
      expect(hangOverhang([copiesOf("a", [mirror("web-2"), offsite, old])])).toBe((2 * 176 + 12 - 288) / 2);
      expect(hangOverhang([copiesOf("a", [mirror("web-2"), offsite]), copiesOf("b", [mirror("web-3")])])).toBe((3 * 176 + 2 * 12 - 288) / 2);
    });
  });
});
