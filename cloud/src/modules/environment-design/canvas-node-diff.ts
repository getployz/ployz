import type { EnvironmentNodeLifecycle } from "#/modules/environment-design/environment-change-set";
import type { DiffRow, ServiceDeploymentDiffKind } from "#/modules/services/service-deployment-diff/fields";
import type { ServiceRecord } from "#/modules/environment-design/services";

/** Canvas-only presentation shape for an explicit Environment Change Set group. */
export type CanvasNodeDiffGroup = {
  nodeType: "service" | "variable_group" | "volume";
  nodeId: string;
  nodeName: string;
  summaryLabel: string;
  lifecycle: EnvironmentNodeLifecycle;
  rows: DiffRow[];
  canDiscard?: boolean;
  serviceSourceType?: ServiceRecord["source"]["type"];
};

export function getCanvasNodeChangeKind(
  group: CanvasNodeDiffGroup,
): ServiceDeploymentDiffKind {
  if (group.lifecycle === "create") return "add";
  if (group.lifecycle === "delete") return "remove";
  return "update";
}

export function getCanvasNodeDiffGroupCanDiscard(group: CanvasNodeDiffGroup) {
  return group.canDiscard !== false;
}
