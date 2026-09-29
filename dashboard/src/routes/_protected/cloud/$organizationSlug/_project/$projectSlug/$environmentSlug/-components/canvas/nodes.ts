import { Position, type Edge } from "@xyflow/react";
import type { VolumeListing } from "@ployz/sdk";
import type { LiveNode } from "#/modules/branches/use-live-nodes";
import type { VolumeResourceRecord } from "#/modules/environment-design/resources";
import type { EnvironmentServiceVolumeAttachment } from "#/modules/environment-design/service-volume-attachments";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import {
  getCanvasPositionCollectionKey,
} from "#/modules/services/services.collection";
import type { ServiceCanvasPositionRecord } from "#/modules/environment-design/services";
import { extractDisplayRefs } from "#/modules/environment-design/variable-template";
import { SERVICE_NODE_WIDTH, SERVICE_NODE_HEIGHT, SERVICE_NODE_SIZE, SNAP_GRID } from "./constants";
import { findPlacement } from "../../-utils/node-placement";
import type {
  CanvasResourceType,
  CanvasStoreServiceNode,
  CanvasStoreVolumeNode,
  StoreCanvas,
  StoreLiveNode,
  CanvasStoreLiveNode,
  CanvasLiveNode,
  CanvasResourceNode,
  CanvasServiceNode,
  CanvasVolumeNode,
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
] satisfies NonNullable<CanvasServiceNode["handles"]>;

function getCanvasPosition(
  position: ServiceCanvasPositionRecord | null | undefined,
) {
  return {
    x: position?.x ?? 0,
    y: position?.y ?? 0,
  };
}

function getPositionByCanvasResource(
  positions: ServiceCanvasPositionRecord[],
) {
  return new Map(
    positions.map((position) => [
      getCanvasPositionCollectionKey(position),
      position,
    ]),
  );
}

function toServiceNode(
  service: EnvironmentServiceViewRecord,
  position: ServiceCanvasPositionRecord | null | undefined,
): CanvasServiceNode {
  return {
    id: service.service.id,
    type: "service",
    position: getCanvasPosition(position),
    width: SERVICE_NODE_WIDTH,
    height: SERVICE_NODE_HEIGHT,
    handles: CANVAS_NODE_HANDLES,
    draggable: true,
    data: {
      resourceType: "service",
      resourceId: service.service.id,
      serviceId: service.service.id,
      environmentId: service.service.environmentId,
    },
  };
}

function toVolumeNode(
  resource: VolumeResourceRecord,
  position: ServiceCanvasPositionRecord | null | undefined,
): CanvasVolumeNode {
  return {
    id: resource.resource.id,
    type: "volume",
    position: getCanvasPosition(position),
    width: SERVICE_NODE_WIDTH,
    height: SERVICE_NODE_HEIGHT,
    handles: CANVAS_NODE_HANDLES,
    draggable: true,
    data: {
      resourceType: "volume",
      resourceId: resource.resource.id,
      environmentId: resource.resource.environmentId,
    },
  };
}

export function buildNodes(
  services: EnvironmentServiceViewRecord[],
  canvasPositions: ServiceCanvasPositionRecord[],
  selectedNodeId: string | null,
  volumeResources: VolumeResourceRecord[] = [],
): CanvasResourceNode[] {
  const positionByResource = getPositionByCanvasResource(canvasPositions);
  const serviceNodes = services.map((service) => {
    return {
      ...toServiceNode(
        service,
        positionByResource.get(
          getCanvasPositionCollectionKey({
            resourceType: "service",
            resourceId: service.service.id,
          }),
        ),
      ),
      selected: service.service.id === selectedNodeId,
    };
  });

  const volumeNodes = volumeResources.map((resource) => ({
    ...toVolumeNode(
      resource,
      positionByResource.get(
        getCanvasPositionCollectionKey({
          resourceType: "volume",
          resourceId: resource.resource.id,
        }),
      ),
    ),
    selected: resource.resource.id === selectedNodeId,
  }));

  return [...serviceNodes, ...volumeNodes];
}

/**
 * Config Store Services and Volumes where the canvas last put them; positions stay Cloud's, keyed by the node's id.
 * One made elsewhere (the CLI) has no place yet: the first free one near the origin, or below the first Service that
 * mounts a Volume, until someone drags it.
 */
export function buildStoreNodes(
  store: Pick<StoreCanvas, "services" | "volumes"> & { live?: StoreLiveNode[] },
  canvasPositions: ServiceCanvasPositionRecord[],
  selectedNodeId: string | null,
  environmentId: string,
): (CanvasStoreServiceNode | CanvasStoreVolumeNode | CanvasStoreLiveNode)[] {
  const positionByResource = getPositionByCanvasResource(canvasPositions);
  const occupied = canvasPositions.map((position) => ({ x: position.x, y: position.y, ...SERVICE_NODE_SIZE }));
  const place = (resourceType: CanvasResourceType, resourceId: string, near = { x: 0, y: 0 }) => {
    const stored = positionByResource.get(getCanvasPositionCollectionKey({ resourceType, resourceId }));
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

/** A link from each Config Store Volume into every Service that mounts it, as legacy mounts drew them. */
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

export function buildEdges(
  volumeResources: VolumeResourceRecord[] = [],
  volumeAttachments: EnvironmentServiceVolumeAttachment[] = [],
  services: EnvironmentServiceViewRecord[] = [],
): Edge[] {
  const serviceIdBySlug = new Map(
    services.map((service) => [service.service.slug, service.service.id]),
  );
  const referencePairs = new Set<string>();
  const referenceEdges: Edge[] = [];
  for (const service of services) {
    const consumerServiceId = service.service.id;
    for (const variable of service.variables) {
      if (variable.value.type !== "plain") {
        continue;
      }
      for (const ref of extractDisplayRefs(variable.value.value)) {
        if (ref.ownerSlug == null) {
          continue;
        }
        const producerId = serviceIdBySlug.get(ref.ownerSlug);
        if (!producerId || producerId === consumerServiceId) {
          continue;
        }
        const pairKey = `${producerId}:${consumerServiceId}`;
        if (referencePairs.has(pairKey)) {
          continue;
        }
        referencePairs.add(pairKey);
        // Producer renders below its consumer: the producer's top (exit) flows
        // into the consumer's bottom (entry), arrow pointing at the consumer —
        // data flows from the referenced producer into the service.
        referenceEdges.push({
          id: `reference:${pairKey}`,
          source: producerId,
          target: consumerServiceId,
        });
      }
    }
  }

  // Mount edges connect a volume node to each consuming service. Only authored
  // volumes render edges.
  const activeVolumeIds = new Set(
    volumeResources.flatMap((volume) =>
      volume.isAuthored ? [volume.resource.id] : [],
    ),
  );
  // Volumes render below their service: the volume's top (exit) flows into the
  // service's bottom (entry), arrow pointing into the consuming service.
  const volumeEdges = volumeAttachments.flatMap((attachment) =>
    activeVolumeIds.has(attachment.volumeResourceId)
      ? [{
          id: `mount:${attachment.volumeResourceId}:${attachment.serviceId}`,
          source: attachment.volumeResourceId,
          target: attachment.serviceId,
        }]
      : [],
  );

  return [...referenceEdges, ...volumeEdges];
}

/** Links into Live Nodes are dashed; every other link is solid. */
export const LIVE_EDGE_STYLE = { stroke: "var(--muted-foreground)", strokeDasharray: "6 4" };

export const liveNodeId = (lineageId: string) => `live:${lineageId}`;

/** A Branch's Live Nodes, each where it sits on its owner's canvas. */
export function buildLiveNodes(
  liveNodes: LiveNode[],
  canvasPositions: ServiceCanvasPositionRecord[],
): CanvasLiveNode[] {
  const positionByResource = getPositionByCanvasResource(canvasPositions);
  return liveNodes.map((liveNode) => ({
    id: liveNodeId(liveNode.lineageId),
    type: "live",
    position: getCanvasPosition(liveNode.owner
      ? positionByResource.get(getCanvasPositionCollectionKey({ resourceType: "service", resourceId: liveNode.owner.node.nodeId }))
      : null),
    width: SERVICE_NODE_WIDTH,
    height: SERVICE_NODE_HEIGHT,
    handles: CANVAS_NODE_HANDLES,
    draggable: false,
    data: { liveNode },
  }));
}

/** Dashed links from each Live Node into the services here that use it. */
export function buildLiveEdges(liveNodes: LiveNode[]): Edge[] {
  return liveNodes.flatMap((liveNode) => liveNode.usedBy.map((serviceId) => ({
    id: `${liveNodeId(liveNode.lineageId)}:${serviceId}`,
    source: liveNodeId(liveNode.lineageId),
    target: serviceId,
    style: LIVE_EDGE_STYLE,
  })));
}
