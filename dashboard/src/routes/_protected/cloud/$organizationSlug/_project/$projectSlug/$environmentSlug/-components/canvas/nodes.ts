import { Position, type Edge } from "@xyflow/react";
import type { VolumeListing } from "@ployz/sdk";
import { canvasPositionKey, type CanvasPosition } from "#/modules/canvas/canvas-positions";
import { SERVICE_NODE_WIDTH, SERVICE_NODE_HEIGHT, SERVICE_NODE_SIZE, VOLUME_TRAY_HEIGHT } from "./constants";
import { findPlacement } from "../../-utils/node-placement";
import type {
  CanvasResourceNode,
  CanvasResourceType,
  CanvasStoreServiceNode,
  CanvasStoreVolumeNode,
  StoreCanvas,
  StoreCanvasService,
  StoreLiveNode,
  CanvasStoreLiveNode,
  VolumeTray,
} from "./types";

/** A node's link handles: out of its top, into its bottom, below any trays. */
const handles = (height: number) => [
  { type: "source", position: Position.Top, x: SERVICE_NODE_WIDTH / 2, y: 0 },
  { type: "target", position: Position.Bottom, x: SERVICE_NODE_WIDTH / 2, y: height },
] satisfies NonNullable<CanvasStoreServiceNode["handles"]>;

function getCanvasPosition(
  position: CanvasPosition | null | undefined,
) {
  return {
    x: position?.x ?? 0,
    y: position?.y ?? 0,
  };
}

function getPositionByCanvasResource(
  positions: CanvasPosition[],
) {
  return new Map(
    positions.map((position) => [
      canvasPositionKey(position),
      position,
    ]),
  );
}

/**
 * Each Volume as a tray under every Service here that mounts it, in the Store's order, keyed by Service id; `unmounted`,
 * the Volumes no Service here mounts, which stay nodes of their own.
 */
export function volumeTrays(services: readonly Pick<StoreCanvasService, "service">[], volumes: readonly VolumeListing[]) {
  const idByName = new Map(services.map(({ service }) => [service.name, service.id]));
  const trays = new Map<string, VolumeTray[]>();
  const unmounted: VolumeListing[] = [];
  for (const volume of volumes) {
    const mounting = [...new Set(volume.mounts.map((mount) => mount.service))].filter((name) => idByName.has(name));
    if (mounting.length === 0) unmounted.push(volume);
    for (const name of mounting) {
      const id = idByName.get(name) ?? name;
      trays.set(id, [...trays.get(id) ?? [], { volume, sharedWith: mounting.filter((other) => other !== name) }]);
    }
  }
  return { trays, unmounted };
}

/**
 * Config Store Services and Volumes where the canvas last put them; positions stay Cloud's, keyed by the node's id.
 * One made elsewhere (the CLI) has no place yet: the first free one near the origin, until someone drags it. A Volume
 * a Service here mounts is a tray under it, not a node.
 */
export function buildStoreNodes(
  store: Pick<StoreCanvas, "services" | "volumes"> & { live?: StoreLiveNode[] },
  canvasPositions: CanvasPosition[],
  selectedNodeId: string | null,
  environmentId: string,
): (CanvasStoreServiceNode | CanvasStoreVolumeNode | CanvasStoreLiveNode)[] {
  const { trays, unmounted } = volumeTrays(store.services, store.volumes);
  // A Service's node grows by its trays; placing one keeps clear of each node's whole height.
  const sizeOf = (id: string) => ({ ...SERVICE_NODE_SIZE, height: SERVICE_NODE_HEIGHT + (trays.get(id)?.length ?? 0) * VOLUME_TRAY_HEIGHT });
  const positionByResource = getPositionByCanvasResource(canvasPositions);
  const occupied = canvasPositions.map((position) => ({ x: position.x, y: position.y, ...sizeOf(position.resourceId) }));
  const place = (resourceType: CanvasResourceType, resourceId: string) => {
    const stored = positionByResource.get(canvasPositionKey({ resourceType, resourceId }));
    if (stored) return getCanvasPosition(stored);
    const size = sizeOf(resourceId);
    const position = findPlacement({ x: 0, y: 0 }, size, occupied);
    occupied.push({ ...position, ...size });
    return position;
  };
  const node = { width: SERVICE_NODE_WIDTH, height: SERVICE_NODE_HEIGHT, handles: handles(SERVICE_NODE_HEIGHT), draggable: true };
  return [
    ...store.services.map((service) => {
      const { height } = sizeOf(service.service.id);
      return {
        ...node,
        height,
        handles: handles(height),
        id: service.service.id,
        type: "storeService",
        position: place("service", service.service.id),
        selected: service.service.id === selectedNodeId,
        data: { ...service, trays: trays.get(service.service.id) ?? [], resourceType: "service", resourceId: service.service.id, environmentId },
      } satisfies CanvasStoreServiceNode;
    }),
    ...unmounted.map((volume) => ({
      ...node,
      id: volume.id,
      type: "storeVolume",
      position: place("volume", volume.id),
      selected: volume.id === selectedNodeId,
      data: { volume, resourceType: "volume", resourceId: volume.id, environmentId },
    } satisfies CanvasStoreVolumeNode)),
    // A Branch's Live Nodes take free spots; they are their owner's to move.
    ...(store.live ?? []).map((live) => ({
      ...node,
      id: liveNodeId(live.name),
      type: "storeLive",
      position: place("service", liveNodeId(live.name)),
      draggable: false,
      selected: liveNodeId(live.name) === selectedNodeId,
      data: { live },
    } satisfies CanvasStoreLiveNode)),
  ];
}

/** Dashed links from each Live Node into the Services here that read it; a mount is a tray, not a link. */
export function buildStoreEdges(store: { live?: StoreLiveNode[] }): Edge[] {
  return (store.live ?? []).flatMap((live) => live.usedBy.map((serviceId) => ({
    id: `${liveNodeId(live.name)}:${serviceId}`, source: liveNodeId(live.name), target: serviceId, style: LIVE_EDGE_STYLE,
  })));
}

/** The node that shows `id` on the canvas: the node itself, else the first Service with it as a tray; null when none does. */
export function canvasNodeOf(nodes: readonly CanvasResourceNode[], id: string) {
  const shown = nodes.find((node) => node.id === id)
    ?? nodes.find((node) => node.type === "storeService" && node.data.trays.some((tray) => tray.volume.id === id));
  return shown?.id ?? null;
}

/** Links into Live Nodes are dashed. */
export const LIVE_EDGE_STYLE = { stroke: "var(--muted-foreground)", strokeDasharray: "6 4" };

export const liveNodeId = (lineageId: string) => `live:${lineageId}`;
