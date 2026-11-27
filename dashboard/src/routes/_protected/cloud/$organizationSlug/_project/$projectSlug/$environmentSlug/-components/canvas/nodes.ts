import { volumeWriters } from "#/modules/config-store/volume-sharing";
import { Position, type Edge } from "@xyflow/react";
import type { DiffView, ServiceListing, VolumeListing } from "@ployz/sdk";
import { serviceChanges } from "#/modules/config-store/store-services";
import { canvasPositionKey, type CanvasPlacement, type CanvasPosition } from "#/modules/canvas/canvas-positions";
import { COPY_CARD_GAP, COPY_CARD_WIDTH, HANG_HEIGHT, SERVICE_NODE_WIDTH, SERVICE_NODE_HEIGHT, SERVICE_NODE_SIZE, VOLUME_TRAY_HEIGHT } from "./constants";
import { placeNewNodes } from "../../-utils/node-placement";
import { parseMirrorResourceId } from "#/modules/volume-placement/volume-placement";
import { hangingCopies, type VolumeCopies } from "#/modules/volume-copies/volume-copies";
import type {
  CanvasResourceNode,
  CanvasStoreServiceNode,
  CanvasStoreVolumeNode,
  CanvasStoreConfigNode,
  MountedVolume,
  StoreCanvas,
  StoreCanvasService,
  StoreLiveNode,
  CanvasStoreLiveNode,
  PendingNode,
  ArrivingNode,
  ServiceEdge,
  ServiceEdgeData,
} from "./types";
import type { Arriving } from "#/modules/config-store/store-pending";
import { mountSetting } from "#/modules/config-store/setting-paths";

/** A node's link handles: out of its top, into its bottom, below any trays. */
const handles = (height: number) => [
  { type: "source", position: Position.Top, x: SERVICE_NODE_WIDTH / 2, y: 0 },
  { type: "target", position: Position.Bottom, x: SERVICE_NODE_WIDTH / 2, y: height },
] satisfies NonNullable<CanvasStoreServiceNode["handles"]>;

/**
 * Each Volume as a tray under every Service here that mounts it, in the Store's order, keyed by Service id, marked where
 * `diff` stages that Service's mount of it; `unmounted`, the Volumes no Service here mounts, which stay nodes of their own.
 */
export function volumeTrays(services: readonly Pick<ServiceListing, "id" | "name">[], volumes: readonly VolumeListing[], diff: DiffView,
  replicasOf: (service: string) => number = () => 1, copies: ReadonlyMap<string, VolumeCopies> = new Map()) {
  const names = new Set(services.map((service) => service.name));
  const mounted = volumes.map((volume) => ({ volume, by: [...new Set(volume.mounts.map((mount) => mount.service))].filter((name) => names.has(name)) }));
  return {
    trays: new Map(services.map((service) => {
      // A mount is the Service's Setting, `mounts.VOLUME`.
      const changes = serviceChanges(diff, service.id);
      return [service.id, mounted.flatMap(({ volume, by }): MountedVolume[] => by.includes(service.name)
        ? [{ volume, sharedWith: by.filter((name) => name !== service.name), mountChanged: changes.has(mountSetting(volume.name)),
          writers: volumeWriters(volume, replicasOf).total, copies: copies.get(volume.id) ?? null }] : [])];
    })),
    unmounted: mounted.flatMap(({ volume, by }) => by.length === 0 ? [volume] : []),
  };
}

type CanvasStore = Pick<StoreCanvas, "services" | "unmountedVolumes" | "unmountedConfigs"> & Partial<Pick<StoreCanvas, "live" | "copies" | "arriving">>;

/** A node grows by its trays and the row of copies under them; placing one keeps clear of each node's whole height. */
function nodeSizes(store: CanvasStore) {
  const trayCount = new Map(store.services.map(({ service, trays, configTrays }) => [service.id, trays.length + configTrays.length]));
  const hangOf = hangByNode(store);
  return {
    hangOf,
    sizeOf: (id: string) => ({
      ...SERVICE_NODE_SIZE,
      height: SERVICE_NODE_HEIGHT + (trayCount.get(id) ?? 0) * VOLUME_TRAY_HEIGHT + (hangOf.has(id) ? HANG_HEIGHT : 0),
    }),
  };
}

/**
 * What a Live Node's place is kept under. Places sync per Organization by type and id, and a Live Node's name repeats
 * across Branches, so its Branch is part of it.
 */
const livePlaceId = (environmentId: string, name: string) => `${environmentId}/${name}`;

/** Every node the canvas draws, by what its place is kept under, in drawing order. */
function drawnPlaces(store: CanvasStore, environmentId: string): Pick<CanvasPlacement, "resourceType" | "resourceId">[] {
  return [
    ...store.services.map(({ service }) => ({ resourceType: "service" as const, resourceId: service.id })),
    ...store.unmountedVolumes.map(({ id }) => ({ resourceType: "volume" as const, resourceId: id })),
    ...store.unmountedConfigs.map(({ id }) => ({ resourceType: "config" as const, resourceId: id })),
    ...(store.live ?? []).map(({ name }) => ({ resourceType: "live" as const, resourceId: livePlaceId(environmentId, name) })),
    ...(store.arriving ?? []).map(({ node, id }) => ({ resourceType: node, resourceId: id })),
  ];
}

/**
 * A place for each node the canvas draws that has none: one made by the CLI, an agent, a Sync, a restore, or a Branch's
 * Live Node. New places go below the nodes already placed, which never move; the canvas keeps them, so each node is
 * placed once. Only the nodes drawn here take space: a position outlives what it placed (a refused create, a deleted
 * node, a Volume or Config now in a tray), and nothing deletes it.
 */
export function canvasPlacements(store: CanvasStore, canvasPositions: readonly CanvasPosition[], environmentId: string): CanvasPlacement[] {
  const { sizeOf } = nodeSizes(store);
  const stored = new Map(canvasPositions.map((position) => [canvasPositionKey(position), position]));
  const drawn = drawnPlaces(store, environmentId);
  const placed = drawn.flatMap((node) => {
    const position = stored.get(canvasPositionKey(node));
    return position ? [{ x: position.x, y: position.y, ...sizeOf(node.resourceId) }] : [];
  });
  const unplaced = drawn.filter((node) => !stored.has(canvasPositionKey(node)));
  const places = placeNewNodes(placed, unplaced.map((node) => sizeOf(node.resourceId)));
  return unplaced.flatMap((node, index) => {
    const place = places[index];
    return place ? [{ ...node, ...place }] : [];
  });
}

/**
 * Config Store Services, Volumes and Configs where the canvas last put them, and a Branch's Live Nodes; positions stay
 * Cloud's, keyed by the node's id. A node without one is drawn where `canvasPlacements` puts it, which the canvas then
 * keeps. A Volume or Config a Service here mounts is a tray under it, not a node. Which node is selected is the route's
 * to say, not React Flow's.
 */
export function buildStoreNodes(
  store: CanvasStore,
  canvasPositions: CanvasPosition[],
  environmentId: string,
): CanvasResourceNode[] {
  const { sizeOf, hangOf } = nodeSizes(store);
  const positions = new Map([...canvasPositions, ...canvasPlacements(store, canvasPositions, environmentId)]
    .map((position) => [canvasPositionKey(position), { x: position.x, y: position.y }]));
  // Every node drawn is placed, stored or new, so a missing place is a node this function forgot to list.
  const place = (resourceType: CanvasPlacement["resourceType"], resourceId: string) =>
    positions.get(canvasPositionKey({ resourceType, resourceId })) ?? { x: 0, y: 0 };
  // React Flow draws a node at its width and height, so that is its measured size too. A drag needs it, and a new node
  // object from this rebuild would otherwise drop the size React Flow measured, since an unchanged size isn't measured again.
  const sized = (height: number) => ({ width: SERVICE_NODE_WIDTH, height, measured: { width: SERVICE_NODE_WIDTH, height }, handles: handles(height) });
  const node = { ...sized(SERVICE_NODE_HEIGHT), draggable: true };
  return [
    ...store.services.map((service) => {
      const { height } = sizeOf(service.service.id);
      return {
        ...node,
        ...sized(height),
        id: service.service.id,
        type: "storeService",
        position: place("service", service.service.id),
        data: { ...service, hang: hangOf.get(service.service.id) ?? [], resourceType: "service", resourceId: service.service.id, environmentId },
      } satisfies CanvasStoreServiceNode;
    }),
    ...store.unmountedVolumes.map((volume) => {
      const { height } = sizeOf(volume.id);
      return {
        ...node,
        ...sized(height),
        id: volume.id,
        type: "storeVolume",
        position: place("volume", volume.id),
        data: { volume, hang: hangOf.get(volume.id) ?? [], resourceType: "volume", resourceId: volume.id, environmentId },
      } satisfies CanvasStoreVolumeNode;
    }),
    ...store.unmountedConfigs.map((config) => ({
      ...node,
      id: config.id,
      type: "storeConfig",
      position: place("config", config.id),
      data: { config, resourceType: "config", resourceId: config.id, environmentId },
    } satisfies CanvasStoreConfigNode)),
    // A Branch's Live Nodes keep their first place; they are their owner's to move.
    ...(store.live ?? []).map((live) => ({
      ...node,
      id: liveNodeId(live.name),
      type: "storeLive",
      position: place("live", livePlaceId(environmentId, live.name)),
      draggable: false,
      data: { live },
    } satisfies CanvasStoreLiveNode)),
    // Where its creator put it, which is saved before the Store is asked, so the answer lands in the same spot. It
    // holds still until the Store answers, since a drag before then has no node to save.
    ...(store.arriving ?? []).map((arriving) => ({
      ...SERVICE_NODE_SIZE,
      id: arriving.id,
      type: "pending",
      position: place(arriving.node, arriving.id),
      draggable: false,
      data: { name: arriving.name },
    } satisfies PendingNode)),
  ];
}

/** What this tab is adding that the Store has not answered yet: a Domain arrives on its Service's card, not as a node. */
export function arrivingNodes(arriving: readonly Arriving[], answered: readonly { id: string }[]): ArrivingNode[] {
  const ids = new Set(answered.map((listing) => listing.id));
  return arriving.flatMap(({ node, id, name }) => node !== "domain" && !ids.has(id) ? [{ node, id, name }] : []);
}

/** The copies of each Volume with any to hang, keyed by the node that shows the Volume: its first Service's, else its own. */
export function hangByNode(store: Pick<StoreCanvas, "services"> & Partial<Pick<StoreCanvas, "copies">>) {
  const byNode = new Map<string, VolumeCopies[]>();
  for (const [volumeId, copies] of store.copies ?? []) {
    if (hangingCopies(copies).length === 0) continue;
    const shownBy = store.services.find(({ trays }) => trays.some((tray) => tray.volume.id === volumeId))?.service.id ?? volumeId;
    byNode.set(shownBy, [...byNode.get(shownBy) ?? [], copies]);
  }
  return byNode;
}

/** How far a node's row of copies reaches past each side of its card, centred under it. */
export function hangOverhang(hang: readonly VolumeCopies[]) {
  const cards = hang.reduce((sum, copies) => sum + hangingCopies(copies).length, 0);
  const width = cards * COPY_CARD_WIDTH + Math.max(0, cards - 1) * COPY_CARD_GAP;
  return Math.max(0, (width - SERVICE_NODE_WIDTH) / 2);
}

/** Links into Live Nodes are dashed. */
const LIVE_EDGE_STYLE = { stroke: "var(--muted-foreground)", strokeDasharray: "6 4" };

/** A Live Node by name and the Environment it runs in: "production's redis". */
export const liveName = (live: Pick<StoreLiveNode, "name" | "owner">) => live.owner ? `${live.owner}'s ${live.name}` : live.name;

/**
 * Dashed links from each Live Node into the Services here that read it; a mount is a tray, not a link. Each is named
 * for its ends, "api reads production's redis", not React Flow's ids.
 */
export function buildStoreEdges(store: { live?: StoreLiveNode[]; services?: Pick<StoreCanvasService, "service">[] }): Edge[] {
  const names = new Map((store.services ?? []).map(({ service }) => [service.id, service.name]));
  return (store.live ?? []).flatMap((live) => live.usedBy.map((serviceId) => ({
    id: `${liveNodeId(live.name)}:${serviceId}`, source: liveNodeId(live.name), target: serviceId, style: LIVE_EDGE_STYLE,
    ariaLabel: `${names.get(serviceId) ?? serviceId} reads ${liveName(live)}`,
  })));
}

/** Links between Services are solid, apart from the dashed links into Live Nodes. */
const SERVICE_EDGE_STYLE = { stroke: "var(--muted-foreground)", strokeWidth: 1.5 };

/** Why `reader` links to `read`: "api references postgres.DATABASE_URL", each key it reads. */
export const serviceEdgeReason = ({ reader, read, keys }: ServiceEdgeData) =>
  `${reader} references ${keys.map((key) => `${read}.${key}`).join(", ")}`;

/**
 * Links from each Service into the Services here that read it, from a variable or a file of a Config they mount: the
 * references that order a Deploy. Each carries the keys read, to say why it exists. A Service reading itself, or one
 * not drawn here, links nothing; two Services reading each other link both ways.
 */
export function buildServiceEdges(services: readonly Pick<StoreCanvasService, "service">[]): ServiceEdge[] {
  const names = new Map(services.map(({ service }) => [service.id, service.name]));
  return services.flatMap(({ service: reader }) => (reader.references ?? []).flatMap(({ service: id, keys }) => {
    const read = names.get(id);
    if (read === undefined || id === reader.id) return [];
    const data = { reader: reader.name, read, keys };
    return [{
      id: `service:${id}:${reader.id}`, source: id, target: reader.id, type: "service", style: SERVICE_EDGE_STYLE,
      ariaLabel: serviceEdgeReason(data), data,
    } satisfies ServiceEdge];
  }));
}

/** The node that shows `id` on the canvas: the node itself, else the first Service with it as a tray; null when none does. */
export function canvasNodeOf(nodes: readonly CanvasResourceNode[], id: string): string | null {
  // A mirror hangs inside the node that shows its Volume.
  const mirror = parseMirrorResourceId(id);
  if (mirror !== null) return canvasNodeOf(nodes, mirror.volumeId);
  const shown = nodes.find((node) => node.id === id)
    ?? nodes.find((node) => node.type === "storeService"
      && (node.data.trays.some((tray) => tray.volume.id === id) || node.data.configTrays.some((tray) => tray.config.id === id)));
  return shown?.id ?? null;
}

export const liveNodeId = (lineageId: string) => `live:${lineageId}`;
