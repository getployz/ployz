import { volumeWriters } from "#/modules/config-store/volume-sharing";
import { Position, type Edge } from "@xyflow/react";
import type { DiffView, ServiceListing, VolumeListing } from "@ployz/sdk";
import { serviceChanges } from "#/modules/config-store/store-services";
import { canvasPositionKey, type CanvasPosition } from "#/modules/canvas/canvas-positions";
import { SERVICE_NODE_WIDTH, SERVICE_NODE_HEIGHT, SERVICE_NODE_SIZE, VOLUME_TRAY_HEIGHT } from "./constants";
import { findPlacement } from "../../-utils/node-placement";
import type {
  CanvasResourceNode,
  CanvasResourceType,
  CanvasStoreServiceNode,
  CanvasStoreVolumeNode,
  CanvasStoreConfigNode,
  MountedVolume,
  StoreCanvas,
  StoreLiveNode,
  CanvasStoreLiveNode,
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
 * Each Volume as a tray under every Service here that mounts it, in the Store's order, keyed by Service id, marked where
 * `diff` stages that Service's mount of it; `unmounted`, the Volumes no Service here mounts, which stay nodes of their own.
 */
export function volumeTrays(services: readonly Pick<ServiceListing, "id" | "name">[], volumes: readonly VolumeListing[], diff: DiffView,
  replicasOf: (service: string) => number = () => 1) {
  const names = new Set(services.map((service) => service.name));
  const mounted = volumes.map((volume) => ({ volume, by: [...new Set(volume.mounts.map((mount) => mount.service))].filter((name) => names.has(name)) }));
  return {
    trays: new Map(services.map((service) => {
      // A mount is the Service's Setting, `mounts.VOLUME`.
      const changes = serviceChanges(diff, service.id);
      return [service.id, mounted.flatMap(({ volume, by }): MountedVolume[] => by.includes(service.name)
        ? [{ volume, sharedWith: by.filter((name) => name !== service.name), mountChanged: changes.has(`mounts.${volume.name}`),
          writers: volumeWriters(volume, replicasOf).total }] : [])];
    })),
    unmounted: mounted.flatMap(({ volume, by }) => by.length === 0 ? [volume] : []),
  };
}

/**
 * Config Store Services, Volumes and Configs where the canvas last put them; positions stay Cloud's, keyed by the node's id.
 * One made elsewhere (the CLI) has no place yet: the first free one near the origin, until someone drags it. A Volume
 * or Config a Service here mounts is a tray under it, not a node. Which node is selected is the route's to say, not React Flow's.
 */
export function buildStoreNodes(
  store: Pick<StoreCanvas, "services" | "unmountedVolumes" | "unmountedConfigs"> & { live?: StoreLiveNode[] },
  canvasPositions: CanvasPosition[],
  environmentId: string,
): CanvasResourceNode[] {
  const trayCount = new Map(store.services.map(({ service, trays, configTrays }) => [service.id, trays.length + configTrays.length]));
  // A Service's node grows by its trays; placing one keeps clear of each node's whole height.
  const sizeOf = (id: string) => ({ ...SERVICE_NODE_SIZE, height: SERVICE_NODE_HEIGHT + (trayCount.get(id) ?? 0) * VOLUME_TRAY_HEIGHT });
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
        data: { ...service, resourceType: "service", resourceId: service.service.id, environmentId },
      } satisfies CanvasStoreServiceNode;
    }),
    ...store.unmountedVolumes.map((volume) => ({
      ...node,
      id: volume.id,
      type: "storeVolume",
      position: place("volume", volume.id),
      data: { volume, resourceType: "volume", resourceId: volume.id, environmentId },
    } satisfies CanvasStoreVolumeNode)),
    ...store.unmountedConfigs.map((config) => ({
      ...node,
      id: config.id,
      type: "storeConfig",
      position: place("config", config.id),
      data: { config, resourceType: "config", resourceId: config.id, environmentId },
    } satisfies CanvasStoreConfigNode)),
    // A Branch's Live Nodes take free spots; they are their owner's to move.
    ...(store.live ?? []).map((live) => ({
      ...node,
      id: liveNodeId(live.name),
      type: "storeLive",
      position: place("service", liveNodeId(live.name)),
      draggable: false,
      data: { live },
    } satisfies CanvasStoreLiveNode)),
  ];
}

/** Links into Live Nodes are dashed. */
const LIVE_EDGE_STYLE = { stroke: "var(--muted-foreground)", strokeDasharray: "6 4" };

/** Dashed links from each Live Node into the Services here that read it; a mount is a tray, not a link. */
export function buildStoreEdges(store: { live?: StoreLiveNode[] }): Edge[] {
  return (store.live ?? []).flatMap((live) => live.usedBy.map((serviceId) => ({
    id: `${liveNodeId(live.name)}:${serviceId}`, source: liveNodeId(live.name), target: serviceId, style: LIVE_EDGE_STYLE,
  })));
}

/** The node that shows `id` on the canvas: the node itself, else the first Service with it as a tray; null when none does. */
export function canvasNodeOf(nodes: readonly CanvasResourceNode[], id: string) {
  const shown = nodes.find((node) => node.id === id)
    ?? nodes.find((node) => node.type === "storeService"
      && (node.data.trays.some((tray) => tray.volume.id === id) || node.data.configTrays.some((tray) => tray.config.id === id)));
  return shown?.id ?? null;
}

/** The nodes that show `ids` on the canvas, each once: what an open Deployment Page brings into view. */
export function shownNodeIds(nodes: readonly CanvasResourceNode[], ids: readonly string[]) {
  return [...new Set(ids.flatMap((id) => canvasNodeOf(nodes, id) ?? []))];
}

export const liveNodeId = (lineageId: string) => `live:${lineageId}`;
