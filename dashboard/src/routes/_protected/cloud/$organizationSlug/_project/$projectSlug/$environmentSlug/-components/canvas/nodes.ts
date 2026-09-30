import { Position, type Edge } from "@xyflow/react";
import type { VolumeListing } from "@ployz/sdk";
import { canvasPositionKey, type CanvasPosition } from "#/modules/canvas/canvas-positions";
import { SERVICE_NODE_WIDTH, SERVICE_NODE_HEIGHT, SERVICE_NODE_SIZE, SNAP_GRID } from "./constants";
import { findPlacement } from "../../-utils/node-placement";
import type {
  CanvasResourceType,
  CanvasStoreServiceNode,
  CanvasStoreVolumeNode,
  StoreCanvas,
  StoreLiveNode,
  CanvasStoreLiveNode,
} from "./types";

const CANVAS_NODE_HANDLES = [
  {
    type: "source",
    position: Position.Top,
    x: SERVICE_NODE_WIDTH / 2,
    y: 0,
  },
  {
    type: "target",
    position: Position.Bottom,
    x: SERVICE_NODE_WIDTH / 2,
    y: SERVICE_NODE_HEIGHT,
  },
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
 * Config Store Services and Volumes where the canvas last put them; positions stay Cloud's, keyed by the node's id.
 * One made elsewhere (the CLI) has no place yet: the first free one near the origin, or below the first Service that
 * mounts a Volume, until someone drags it.
 */
export function buildStoreNodes(
  store: Pick<StoreCanvas, "services" | "volumes"> & { live?: StoreLiveNode[] },
  canvasPositions: CanvasPosition[],
  selectedNodeId: string | null,
  environmentId: string,
): (CanvasStoreServiceNode | CanvasStoreVolumeNode | CanvasStoreLiveNode)[] {
  const positionByResource = getPositionByCanvasResource(canvasPositions);
  const occupied = canvasPositions.map((position) => ({ x: position.x, y: position.y, ...SERVICE_NODE_SIZE }));
  const place = (resourceType: CanvasResourceType, resourceId: string, near = { x: 0, y: 0 }) => {
    const stored = positionByResource.get(canvasPositionKey({ resourceType, resourceId }));
    if (stored) return getCanvasPosition(stored);
    const position = findPlacement(near, SERVICE_NODE_SIZE, occupied);
    occupied.push({ ...position, ...SERVICE_NODE_SIZE });
    return position;
  };
  const node = { width: SERVICE_NODE_WIDTH, height: SERVICE_NODE_HEIGHT, handles: CANVAS_NODE_HANDLES, draggable: true };
  const services = store.services.map((service) => ({
    ...node,
    id: service.service.id,
    type: "storeService",
    position: place("service", service.service.id),
    selected: service.service.id === selectedNodeId,
    data: { ...service, resourceType: "service", resourceId: service.service.id, environmentId },
  } satisfies CanvasStoreServiceNode));
  // Volumes sit below the Services that mount them, so their links run up into them.
  const below = (volume: VolumeListing) => {
    const owner = services.find((service) => service.data.service.name === volume.mounts[0]?.service);
    return owner ? { x: owner.position.x, y: owner.position.y + SERVICE_NODE_HEIGHT + SNAP_GRID[1] * 3 } : undefined;
  };
  return [
    ...services,
    ...store.volumes.map((volume) => ({
      ...node,
      id: volume.id,
      type: "storeVolume",
      position: place("volume", volume.id, below(volume)),
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

/** A link from each Volume into every Service that mounts it, and dashed ones from Live Nodes into their users. */
export function buildStoreEdges(store: Pick<StoreCanvas, "services" | "volumes"> & { live?: StoreLiveNode[] }): Edge[] {
  const serviceIdByName = new Map(store.services.map(({ service }) => [service.name, service.id]));
  return [...store.volumes.flatMap((volume) => volume.mounts.flatMap((mount) => {
    const serviceId = serviceIdByName.get(mount.service);
    return serviceId ? [{ id: `mount:${volume.id}:${serviceId}`, source: volume.id, target: serviceId }] : [];
  })),
  // Dashed links from each Live Node into the Services here that read it.
  ...(store.live ?? []).flatMap((live) => live.usedBy.map((serviceId) => ({
    id: `${liveNodeId(live.name)}:${serviceId}`, source: liveNodeId(live.name), target: serviceId, style: LIVE_EDGE_STYLE,
  })))];
}

/** Links into Live Nodes are dashed; every other link is solid. */
export const LIVE_EDGE_STYLE = { stroke: "var(--muted-foreground)", strokeDasharray: "6 4" };

export const liveNodeId = (lineageId: string) => `live:${lineageId}`;
