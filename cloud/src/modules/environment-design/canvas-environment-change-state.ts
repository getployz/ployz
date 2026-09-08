import type { ReviewAggregateDiscardPlan } from "@ployz/sdk/config";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
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
  type CanvasNodeDiffGroup,
} from "#/modules/environment-design/canvas-node-diff";

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

export type CanvasDiscardAllPlan = ReviewAggregateDiscardPlan;

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
    discardAllPlan: changeSet.discardAllPlan,
    totalCount:
      unsaved.totalCount + pending.totalCount + drift.totalCount,
    // Deploy admission uses the direct Runtime Watch preflight at action time.
    canDeploy: true,
    deploymentEvidence: input.deploymentEvidence,
  };
}
