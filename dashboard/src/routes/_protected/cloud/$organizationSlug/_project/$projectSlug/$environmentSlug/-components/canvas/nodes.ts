import { Position, type Edge } from "@xyflow/react";
import type { VolumeResourceRecord } from "#/modules/environment-design/resources";
import type { EnvironmentServiceVolumeAttachment } from "#/modules/environment-design/service-volume-attachments";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import {
  getCanvasPositionCollectionKey,
} from "#/modules/services/services.collection";
import type { ServiceCanvasPositionRecord } from "#/modules/environment-design/services";
import { extractDisplayRefs } from "#/modules/environment-design/variable-template";
import { SERVICE_NODE_WIDTH, SERVICE_NODE_HEIGHT } from "./constants";
import type {
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
