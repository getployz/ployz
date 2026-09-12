import {
  buildCanvasEnvironmentChangeState,
  type CanvasEnvironmentChangeGroup,
} from "#/modules/environment-design/canvas-environment-change-state";
import type {
  EnvironmentNodeIntroductionProjection,
  EnvironmentNodeProjection,
  EnvironmentStateProjection,
} from "#/modules/environment-design/environment-change-set";
import type { EnvironmentNodeIntroduction } from "#/modules/environment-design/environment-node-introductions";
import type {
  VariableGroupResourceRecord,
  VolumeResourceRecord,
} from "#/modules/environment-design/resources";
import { projectVariableGroupConfig } from "#/modules/environment-design/variable-group-config";
import { namedVolumeConfig } from "#/modules/environment-design/volume-config";
import type {
  EnvironmentChangeStateNodeProjection,
  EnvironmentChangeStateProjection,
} from "#/modules/deployments/deployment-contract";
import { projectServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { EnvironmentServiceViewRecord } from "#/modules/services/services.collection";
import { projectDestructiveEnvironmentSave } from "#/modules/environment-design/working-state-review";
import type { CanvasResourceNode } from "./types";

type UseCanvasFlowStateInput = {
  environmentNamespace: string;
  servicesWithBoundEnv: EnvironmentServiceViewRecord[];
  environmentResources: VariableGroupResourceRecord[];
  volumeResources: VolumeResourceRecord[];
  environmentChangeState: EnvironmentChangeStateProjection | null;
  nodeIntroductions: EnvironmentNodeIntroduction[];
  canvasNodes: CanvasResourceNode[];
  selectedNodeId: string | null;
};

function projectedNode(
  node: EnvironmentChangeStateNodeProjection,
): EnvironmentNodeProjection {
  // SAFETY: EnvironmentChangeStateNodeProjection is a discriminated union; TypeScript cannot correlate nodeType with config.
  return {
    node: { type: node.nodeType, id: node.nodeId },
    config: node.config,
  } as EnvironmentNodeProjection;
}

function countGroup(group: CanvasEnvironmentChangeGroup) {
  return group.changeCount;
}

function countGroupsByNode(groups: CanvasEnvironmentChangeGroup[]) {
  const counts = new Map<string, number>();
  for (const group of groups) {
    counts.set(group.nodeId, (counts.get(group.nodeId) ?? 0) + countGroup(group));
  }
  return counts;
}

export function useCanvasFlowState({
  environmentNamespace,
  servicesWithBoundEnv,
  environmentResources,
  volumeResources,
  environmentChangeState,
  nodeIntroductions,
  canvasNodes,
  selectedNodeId,
}: UseCanvasFlowStateInput) {
  const workingNodes: EnvironmentNodeProjection[] = [
    ...servicesWithBoundEnv.map(
      ({ service }) =>
        ({
          node: { type: "service", id: service.id },
          config:
            service.deletedAt === null
              ? projectServiceDeploymentConfig(service)
              : null,
        }) satisfies EnvironmentNodeProjection,
    ),
    ...environmentResources.map(
      (resource) =>
        ({
          node: { type: "variable_group", id: resource.resource.id },
          config:
            resource.resource.deletedAt === null
              ? projectVariableGroupConfig(resource)
              : null,
        }) satisfies EnvironmentNodeProjection,
    ),
    ...volumeResources.map(
      (resource) =>
        ({
          node: { type: "volume", id: resource.resource.id },
          config:
            resource.isAuthored
              ? namedVolumeConfig(resource.resource.name)
              : null,
        }) satisfies EnvironmentNodeProjection,
    ),
  ];
  const working: EnvironmentStateProjection = {
    token: [
      ...servicesWithBoundEnv.map(
        ({ service }) => `service:${service.id}:${service.updatedAt.toISOString()}`,
      ),
      ...environmentResources.map(
        ({ resource }) =>
          `variable_group:${resource.id}:${resource.updatedAt.toISOString()}`,
      ),
      ...volumeResources.map(
        ({ resource }) =>
          `volume:${resource.id}:${resource.updatedAt.toISOString()}`,
      ),
    ]
      .sort()
      .join("|"),
    nodes: workingNodes,
  };
  const saved: EnvironmentStateProjection =
    environmentChangeState?.saved
      ? {
          token: environmentChangeState.saved.token,
          nodes: environmentChangeState.saved.nodes.map(projectedNode),
        }
      : {
          token: `saved:none:${environmentNamespace}`,
          nodes: [],
        };
  const applied: EnvironmentStateProjection = {
    token:
      environmentChangeState?.applied.token ??
      "applied:none",
    nodes: environmentChangeState?.applied.nodes.map(projectedNode) ?? [],
  };
  const introductions = {
    token: nodeIntroductions
      .map(
        (introduction) =>
          `${introduction.nodeType}:${introduction.nodeId}:${introduction.updatedAt.toISOString()}`,
      )
      .sort()
      .join("|"),
    nodes: nodeIntroductions.map(
      (introduction) =>
        // SAFETY: node introductions pair nodeType with a matching config; TypeScript cannot correlate those fields.
        ({
          node: {
            type: introduction.nodeType,
            id: introduction.nodeId,
          },
          config: introduction.config,
        }) as EnvironmentNodeIntroductionProjection,
    ),
  };
  const deploymentEvidence = environmentChangeState?.deploymentEvidence
    ? {
        id: environmentChangeState.deploymentEvidence.id,
        status: environmentChangeState.deploymentEvidence.status,
        token: environmentChangeState.deploymentEvidence.token,
        // SAFETY: evidence nodes are the same discriminated union as working/applied; TypeScript cannot correlate nodeType with config.
        nodes: environmentChangeState.deploymentEvidence.nodes.map((node) => ({
          node: { type: node.nodeType, id: node.nodeId },
          config: node.config,
        })) as EnvironmentNodeProjection[],
      }
    : null;
  const canvasChangeState = buildCanvasEnvironmentChangeState({
    working,
    saved,
    applied,
    nodeIntroductions: introductions,
    deploymentEvidence,
    nodes: [
      ...servicesWithBoundEnv.map(({ service }) => ({
        node: { type: "service" as const, id: service.id },
        name: service.name,
        summaryLabel: service.name,
        serviceSourceType: service.source.type,
      })),
      ...environmentResources.map((resource) => ({
        node: {
          type: "variable_group" as const,
          id: resource.resource.id,
        },
        name: resource.resource.name,
        summaryLabel: "Variable Group",
      })),
      ...volumeResources.map((resource) => ({
        node: { type: "volume" as const, id: resource.resource.id },
        name: resource.resource.name,
        summaryLabel: "Volume",
      })),
    ],
  });
  const diffGroups = canvasChangeState.groups;
  const countByNodeId = countGroupsByNode(diffGroups);
  const recordedNodeIds = new Set([
    ...(environmentChangeState?.saved?.nodes.map((node) => node.nodeId) ?? []),
    ...(environmentChangeState?.applied.nodes.map((node) => node.nodeId) ?? []),
  ]);
  const evidenceNodeIds = new Set(
    environmentChangeState?.deploymentEvidence?.nodes.map(
      (node) => node.nodeId,
    ) ?? [],
  );
  const servicesById = new Map(
    servicesWithBoundEnv.map((service) => [
      service.service.id,
      {
        serviceView: service,
        diffRowCount: countByNodeId.get(service.service.id) ?? 0,
        hasRecordedTargetSnapshot: recordedNodeIds.has(service.service.id),
        latestDeploymentStatus: evidenceNodeIds.has(service.service.id)
          ? (environmentChangeState?.deploymentEvidence?.status ?? null)
          : null,
      },
    ]),
  );
  const selectedNode = selectedNodeId
    ? canvasNodes.find((node) => node.id === selectedNodeId)
    : null;
  const selectedNodePositionKey = selectedNode
    ? `${selectedNode.position.x}:${selectedNode.position.y}`
    : null;
  const environmentResourcesById = new Map(
    environmentResources.map((resource) => [
      resource.resource.id,
      {
        resource,
        diffRowCount: countByNodeId.get(resource.resource.id) ?? 0,
      },
    ]),
  );
  const volumeResourcesById = new Map(
    volumeResources.map((resource) => [
      resource.resource.id,
      {
        resource,
        diffRowCount: countByNodeId.get(resource.resource.id) ?? 0,
      },
    ]),
  );
  const destructiveSave = projectDestructiveEnvironmentSave({
    workingNodes: working.nodes.map(({ node, config }) => ({
      nodeType: node.type,
      nodeId: node.id,
      config,
    })),
    savedNodes: saved.nodes.map(({ node, config }) => ({
      nodeType: node.type,
      nodeId: node.id,
      config,
    })),
    appliedNodes: applied.nodes.map(({ node, config }) => ({
      nodeType: node.type,
      nodeId: node.id,
      config,
    })),
  });
  const serviceNameById = new Map(
    servicesWithBoundEnv.map(({ service }) => [service.id, service.name]),
  );
  const deployBarPositionClass = selectedNodeId
    ? "inset-x-4 bottom-4 z-20 lg:inset-x-auto lg:left-4"
    : "inset-x-4 bottom-4 z-20 sm:inset-x-auto sm:top-4 sm:bottom-auto sm:left-1/2 sm:-translate-x-1/2";

  return {
    canvasChangeState,
    diffGroups,
    totalChanges: canvasChangeState.totalCount,
    canDeploy: true,
    canSave: canvasChangeState.canSave,
    diffRowCountByServiceId: countByNodeId,
    servicesById,
    selectedNodePositionKey,
    environmentResourcesById,
    volumeResourcesById,
    destructiveServiceIds: destructiveSave.serviceIds,
    destructiveServiceNames: destructiveSave.serviceIds.map(
      (serviceId) => serviceNameById.get(serviceId) ?? serviceId,
    ),
    deletedDeployedVolumeIds: destructiveSave.volumeIds,
    deployBarPositionClass,
  };
}
