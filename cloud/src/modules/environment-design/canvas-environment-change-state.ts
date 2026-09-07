import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import { automaticBoundHostnames } from "#/modules/runtime/runtime";
import type { RuntimeServiceRecord } from "#/modules/runtime/runtime";
import { getManagedHostnameDriftRow } from "#/modules/services/service-deployment-diff/fields";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { ServiceRecord } from "#/modules/environment-design/services";
import type {
  EnvironmentChangeSlice,
  EnvironmentChangeSliceKind,
  EnvironmentNodeDiscardPlan,
  EnvironmentNodeIdentity,
  EnvironmentNodeIntroductionsProjection,
  EnvironmentNodeProjection,
  EnvironmentProjectedNodeChange,
  EnvironmentRuntimeObservationsProjection,
  EnvironmentSavedStateProjection,
  EnvironmentStateProjection,
} from "#/modules/environment-design/environment-change-set";
import { buildEnvironmentChangeSet } from "#/modules/environment-design/environment-change-set";
import {
  getCanvasNodeDiffGroupCanDiscard,
  type CanvasNodeDiffGroup,
} from "#/modules/environment-design/canvas-node-diff";
import type {
  EnvironmentSavedStateDiscardCommand,
  EnvironmentSavedStateDiscardOperation,
} from "#/modules/environment-design/saved-state";

export type CanvasDeploymentEvidence = {
  id: string;
  status: EnvironmentDeploymentStatus;
  token: string;
  nodes: EnvironmentNodeProjection[];
};

export type CanvasEnvironmentNodePresentation = {
  node: EnvironmentNodeIdentity;
  name: string;
  summaryLabel: string;
  serviceSourceType?: ServiceRecord["source"]["type"];
};

export type CanvasEnvironmentChangeGroup = CanvasNodeDiffGroup & {
  slice: EnvironmentChangeSliceKind;
  projectedChange: EnvironmentProjectedNodeChange;
  changeCount: number;
};

export type CanvasEnvironmentChangeSlice = Omit<
  EnvironmentChangeSlice,
  "groups"
> & {
  kind: EnvironmentChangeSliceKind;
  groups: CanvasEnvironmentChangeGroup[];
};

export type CanvasWorkingNodeDiscardPlan =
  Extract<EnvironmentNodeDiscardPlan, { target: "working" }>;

export type CanvasSavedNodeDiscardPlan = Extract<
  EnvironmentNodeDiscardPlan,
  { target: "saved" }
>;

export type CanvasDiscardAllPlan = {
  nodes: Array<{
    node: EnvironmentNodeIdentity;
    working: CanvasWorkingNodeDiscardPlan;
  }>;
  savedCommand: EnvironmentSavedStateDiscardCommand | null;
};

export type CanvasEnvironmentChangeState = {
  slices: Record<EnvironmentChangeSliceKind, CanvasEnvironmentChangeSlice>;
  discardAllPlan: CanvasDiscardAllPlan;
  totalCount: number;
  canDeploy: boolean;
  deploymentEvidence: CanvasDeploymentEvidence | null;
};

export function toCanvasWorkingNodeDiscardPlan(
  plan: EnvironmentNodeDiscardPlan,
): CanvasWorkingNodeDiscardPlan {
  return { kind: plan.kind, target: "working", node: plan.node };
}

function toCanvasSavedNodeDiscardPlan(
  plan: EnvironmentNodeDiscardPlan | null | undefined,
): CanvasSavedNodeDiscardPlan | null {
  if (!plan || plan.target !== "saved") return null;
  return plan;
}

function toSavedNodeDiscardOperation(
  plan: CanvasSavedNodeDiscardPlan,
): EnvironmentSavedStateDiscardOperation {
  return {
    kind: "node",
    nodeType: plan.node.type,
    nodeId: plan.node.id,
  };
}

function buildCanvasDiscardAllPlan(input: {
  unsaved: CanvasEnvironmentChangeSlice;
  pending: CanvasEnvironmentChangeSlice;
}): CanvasDiscardAllPlan {
  const unsavedByNode = new Map(
    input.unsaved.groups
      .filter(getCanvasNodeDiffGroupCanDiscard)
      .map((group) => [`${group.nodeType}:${group.nodeId}`, group]),
  );
  const pendingByNode = new Map(
    input.pending.groups
      .filter(getCanvasNodeDiffGroupCanDiscard)
      .map((group) => [`${group.nodeType}:${group.nodeId}`, group]),
  );
  const keys = [...new Set([...unsavedByNode.keys(), ...pendingByNode.keys()])]
    .sort((left, right) => {
      const rank = (key: string) => key.startsWith("service:") ? 1 : 0;
      return rank(left) - rank(right) || left.localeCompare(right);
    });

  const nodes = keys.flatMap((key) => {
    const pending = pendingByNode.get(key);
    const unsaved = unsavedByNode.get(key);
    const savedPlan = pending?.projectedChange.discardPlan;
    const finalPlan = savedPlan ?? unsaved?.projectedChange.discardPlan;
    if (!finalPlan) return [];
    return [
      {
        node: finalPlan.node,
        working: toCanvasWorkingNodeDiscardPlan(finalPlan),
      },
    ];
  });
  const savedPlans = keys.flatMap((key) => {
    const saved = toCanvasSavedNodeDiscardPlan(
      pendingByNode.get(key)?.projectedChange.discardPlan,
    );
    return saved ? [saved] : [];
  });
  const basis = savedPlans[0]?.basis ?? null;
  if (
    basis &&
    savedPlans.some(
      (plan) =>
        plan.basis.savedStateSnapshotId !== basis.savedStateSnapshotId,
    )
  ) {
    throw new Error("Discard All plans must share one Saved State basis.");
  }

  return {
    nodes,
    savedCommand: basis
      ? {
          kind: "discard",
          basis,
          operations: savedPlans.map(toSavedNodeDiscardOperation),
        }
      : null,
  };
}

export function buildCanvasRuntimeObservations(input: {
  environmentNamespace: string;
  applied: EnvironmentStateProjection;
  runtimeServices: RuntimeServiceRecord[];
  autoDomain: string | null;
  appliedRevisionByNodeId?: Map<string, string | null>;
}): EnvironmentRuntimeObservationsProjection {
  const runtimeByServiceId = new Map(
    input.runtimeServices.flatMap((runtime) =>
      runtime.namespaceId === input.environmentNamespace
        ? [[runtime.serviceId, runtime] as const]
        : [],
    ),
  );
  const settings: EnvironmentRuntimeObservationsProjection["settings"] = [];
  const presence: NonNullable<
    EnvironmentRuntimeObservationsProjection["presence"]
  > = [];
  const matchedRuntimeIds = new Set<string>();

  for (const projectedNode of input.applied.nodes) {
    if (projectedNode.node.type !== "service" || !projectedNode.config) {
      continue;
    }
    // SAFETY: node.type is "service"; config stays the node-config union until this branch.
    const config = projectedNode.config as ServiceDeploymentConfig;
    if (config.source.type === "empty") continue;
    const runtime = runtimeByServiceId.get(config.privateDns) ?? null;
    if (!runtime) {
      presence.push({
        node: projectedNode.node,
        applied: "present",
        observed: "absent",
      });
      continue;
    }
    matchedRuntimeIds.add(runtime.id);
    const appliedRevision = input.appliedRevisionByNodeId?.get(
      projectedNode.node.id,
    );
    if (
      runtime &&
      appliedRevision &&
      runtime.activeRevisionId !== appliedRevision
    ) {
      settings.push({
        node: projectedNode.node,
        setting: "runtime.revision",
        label: "Runtime revision",
        appliedValue: appliedRevision,
        observedValue: runtime.activeRevisionId,
      });
    }
    const observedReplicas = runtime?.instanceCount ?? 0;
    if (observedReplicas !== config.replicas) {
      settings.push({
        node: projectedNode.node,
        setting: "runtime.replicas",
        label: "Replicas",
        appliedValue: String(config.replicas),
        observedValue: String(observedReplicas),
      });
    } else if (
      runtime &&
      runtime.readyInstanceCount < runtime.instanceCount
    ) {
      settings.push({
        node: projectedNode.node,
        setting: "runtime.readyReplicas",
        label: "Ready replicas",
        appliedValue: `${runtime.instanceCount} of ${runtime.instanceCount}`,
        observedValue: `${runtime.readyInstanceCount} of ${runtime.instanceCount}`,
      });
    }

    const managedHostnameDrift = getManagedHostnameDriftRow({
      serviceId: projectedNode.node.id,
      managedHostname: config.managedHostname,
      autoDomain: input.autoDomain,
      boundHostnames: automaticBoundHostnames(runtime),
    });
    if (managedHostnameDrift) {
      settings.push({
        node: projectedNode.node,
        setting: managedHostnameDrift.path,
        label: managedHostnameDrift.label,
        appliedValue: managedHostnameDrift.newValue,
        observedValue: managedHostnameDrift.currentValue,
      });
    }
  }

  for (const runtime of input.runtimeServices) {
    if (
      runtime.namespaceId !== input.environmentNamespace ||
      matchedRuntimeIds.has(runtime.id)
    ) {
      continue;
    }
    presence.push({
      node: { type: "service", id: runtime.serviceId },
      applied: "absent",
      observed: "present",
    });
  }

  return {
    token: input.runtimeServices
      .map((runtime) => `${runtime.id}:${runtime.updatedAt}`)
      .sort()
      .join("|") || "runtime:empty",
    presence,
    settings,
  };
}

function nodeKey(node: EnvironmentNodeIdentity) {
  return `${node.type}:${node.id}`;
}

function presentSlice(input: {
  kind: EnvironmentChangeSliceKind;
  slice: EnvironmentChangeSlice;
  nodesByKey: Map<string, CanvasEnvironmentNodePresentation>;
}): CanvasEnvironmentChangeSlice {
  return {
    ...input.slice,
    kind: input.kind,
    groups: input.slice.groups.map((projectedChange) => {
      const presentation = input.nodesByKey.get(
        nodeKey(projectedChange.node),
      );
      const drift = input.kind === "drift";

      return {
        nodeType: projectedChange.node.type,
        nodeId: projectedChange.node.id,
        nodeName: presentation?.name ?? projectedChange.node.id,
        summaryLabel: presentation?.summaryLabel ?? projectedChange.node.id,
        lifecycle: projectedChange.lifecycle.kind,
        rows: projectedChange.settings.map((setting) => ({
          changeKey: setting.id,
          label: setting.label,
          kind: setting.kind === "drift" ? "update" : setting.kind,
          path: setting.owner.setting,
          currentValue:
            (drift ? setting.targetValue : setting.baselineValue) ?? "",
          newValue:
            (drift ? setting.baselineValue : setting.targetValue) ?? "",
          canDiscard:
            input.kind !== "drift" &&
            projectedChange.node.type === "service" &&
            setting.resettable,
        })),
        canDiscard:
          input.kind !== "drift" && projectedChange.lifecycle.resettable,
        serviceSourceType: presentation?.serviceSourceType,
        slice: input.kind,
        projectedChange,
        changeCount:
          projectedChange.settings.length +
          (projectedChange.lifecycle.kind === "create" ||
          projectedChange.lifecycle.kind === "delete"
            ? 1
            : 0),
      } satisfies CanvasEnvironmentChangeGroup;
    }),
  };
}

export function buildCanvasEnvironmentChangeState(input: {
  working: EnvironmentStateProjection;
  saved: EnvironmentSavedStateProjection;
  applied: EnvironmentStateProjection;
  nodeIntroductions: EnvironmentNodeIntroductionsProjection;
  runtimeObserved: EnvironmentStateProjection | null;
  runtimeObservations?: EnvironmentRuntimeObservationsProjection;
  deploymentEvidence: CanvasDeploymentEvidence | null;
  nodes: CanvasEnvironmentNodePresentation[];
}): CanvasEnvironmentChangeState {
  const changeSet = buildEnvironmentChangeSet({
    working: input.working,
    saved: input.saved,
    applied: input.applied,
    nodeIntroductions: input.nodeIntroductions,
    runtimeObserved: input.runtimeObserved,
    runtimeObservations: input.runtimeObservations,
  });
  const nodesByKey = new Map(
    input.nodes.map((node) => [nodeKey(node.node), node]),
  );
  const unsaved = presentSlice({
    kind: "unsaved",
    slice: changeSet.unsaved,
    nodesByKey,
  });
  const pending = presentSlice({
    kind: "pending",
    slice: changeSet.pending,
    nodesByKey,
  });
  const drift = presentSlice({
    kind: "drift",
    slice: changeSet.drift,
    nodesByKey,
  });

  return {
    slices: { unsaved, pending, drift },
    discardAllPlan: buildCanvasDiscardAllPlan({ unsaved, pending }),
    totalCount:
      unsaved.totalCount + pending.totalCount + drift.totalCount,
    canDeploy: input.runtimeObserved !== null,
    deploymentEvidence: input.deploymentEvidence,
  };
}
