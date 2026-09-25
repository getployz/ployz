import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import { isActiveDeployment } from "#/modules/deployments/runtime-contract";
import type { ServiceRecord } from "./services";
import type {
  EnvironmentNodeIdentity, EnvironmentNodeProjection, EnvironmentStateProjection,
  EnvironmentNodeIntroductionsProjection,
} from "./environment-change-set";
import { buildEnvironmentChangeSet } from "./environment-change-set";
import { presentSettingChange } from "#/modules/services/service-deployment-diff/fields";
import type { CanvasNodeDiffGroup } from "./canvas-node-diff";

export type CanvasDeploymentEvidence = {
  id: string; status: EnvironmentDeploymentStatus; token: string;
  nodes: EnvironmentNodeProjection[];
};
export type CanvasEnvironmentNodePresentation = {
  node: EnvironmentNodeIdentity; name: string; summaryLabel: string;
  serviceSourceType?: ServiceRecord["source"]["type"];
};
export type CanvasEnvironmentChangeGroup = CanvasNodeDiffGroup & { changeCount: number };
export type CanvasEnvironmentChangeState = {
  groups: CanvasEnvironmentChangeGroup[];
  totalCount: number;
  headToken: string;
  canSave: boolean;
};

export function buildCanvasEnvironmentChangeState(input: {
  working: EnvironmentStateProjection;
  saved: EnvironmentStateProjection | null;
  applied: EnvironmentStateProjection;
  nodeIntroductions: EnvironmentNodeIntroductionsProjection;
  deploymentEvidence: CanvasDeploymentEvidence | null;
  nodes: CanvasEnvironmentNodePresentation[];
}): CanvasEnvironmentChangeState {
  const submitted = input.deploymentEvidence && isActiveDeployment(input.deploymentEvidence.status)
    ? input.deploymentEvidence : null;
  const result = buildEnvironmentChangeSet({
    working: input.working, applied: input.applied,
    saved: input.saved,
    submitted: submitted ? { token: submitted.token, nodes: submitted.nodes } : null,
    nodeIntroductions: input.nodeIntroductions,
  });
  const presentations = new Map(input.nodes.map(node => [`${node.node.type}:${node.node.id}`, node]));
  return {
    totalCount: result.totalCount,
    headToken: result.headToken,
    // Publication compares Working with Saved independently of the runtime Head.
    canSave: buildEnvironmentChangeSet({
      working: input.working,
      applied: input.saved ?? { token: "saved:none", nodes: [] },
      saved: input.saved,
      submitted: null,
      nodeIntroductions: input.nodeIntroductions,
    }).totalCount > 0,
    groups: result.groups.map(group => {
      const key = `${group.node.type}:${group.node.id}`;
      const presentation = presentations.get(key);
      return {
        nodeType: group.node.type, nodeId: group.node.id,
        nodeName: presentation?.name ?? group.node.id,
        summaryLabel: presentation?.summaryLabel ?? group.node.id,
        serviceSourceType: presentation?.serviceSourceType,
        lifecycle: group.lifecycle,
        canDiscard: true,
        changeCount: group.settings.length + (group.lifecycle === "update" ? 0 : 1),
        rows: group.settings.map(row => ({
          changeKey: `${key}:${row.path}`,
          path: row.path, kind: row.kind,
          ...presentSettingChange(group.node.type, row.path, row.before, row.after),
          canDiscard: group.node.type === "service" && row.canRestore,
        })),
      };
    }),
  };
}
