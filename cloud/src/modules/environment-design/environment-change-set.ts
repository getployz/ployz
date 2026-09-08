import {
  projectEnvironmentChanges,
  resolveWorkingComparison,
  type ReviewChangeSlice,
} from "@ployz/sdk/config";
import type { EnvironmentResourceNodeConfigByType } from "#/modules/environment-design/environment-resource-node";
import { presentSettingChange } from "#/modules/services/service-deployment-diff/fields";
import { asString } from "#/lib/json";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { EnvironmentSavedStateDiscardCommand } from "#/modules/environment-design/saved-state";

export type EnvironmentNodeLifecycle =
  | "create"
  | "update"
  | "delete"
  | "none";

export type ChangeId = string;

export type EnvironmentNodeIdentity = {
  type: "service" | "variable_group" | "volume";
  id: string;
};

export type EnvironmentProjectedChangeOwner = {
  node: EnvironmentNodeIdentity;
  setting: string;
};

export type DisplayValue = string | null;

export type EnvironmentNodeConfigByType = {
  service: ServiceDeploymentConfig;
} & EnvironmentResourceNodeConfigByType;

export type EnvironmentNodeProjection = {
  [TNodeType in EnvironmentNodeIdentity["type"]]: {
    node: EnvironmentNodeIdentity & { type: TNodeType };
    config: EnvironmentNodeConfigByType[TNodeType] | null;
  };
}[EnvironmentNodeIdentity["type"]];

export type EnvironmentNodeIntroductionProjection = {
  [TNodeType in EnvironmentNodeIdentity["type"]]: {
    node: EnvironmentNodeIdentity & { type: TNodeType };
    config: EnvironmentNodeConfigByType[TNodeType];
  };
}[EnvironmentNodeIdentity["type"]];

export type EnvironmentStateProjection = {
  token: string;
  nodes: EnvironmentNodeProjection[];
};

export type EnvironmentSavedStateProjection =
  | (EnvironmentStateProjection & { kind: "no_saved_state" })
  | (EnvironmentStateProjection & {
      kind: "saved_revision";
      savedStateSnapshotId: string;
    });

export type EnvironmentNodeIntroductionsProjection = {
  token: string;
  nodes: EnvironmentNodeIntroductionProjection[];
};

export type EnvironmentChangeSetProjectionInput = {
  working: EnvironmentStateProjection;
  saved: EnvironmentSavedStateProjection;
  applied: EnvironmentStateProjection;
  nodeIntroductions: EnvironmentNodeIntroductionsProjection;
  runtimeObserved: EnvironmentStateProjection | null;
  runtimeObservations?: EnvironmentRuntimeObservationsProjection;
};

export type EnvironmentRuntimeObservationsProjection = {
  token: string;
  presence?: Array<{
    node: EnvironmentNodeIdentity;
    applied: "present" | "absent";
    observed: "present" | "absent";
  }>;
  settings: Array<{
    node: EnvironmentNodeIdentity;
    setting: string;
    label: string;
    appliedValue: DisplayValue;
    observedValue: DisplayValue;
  }>;
};

export type EnvironmentProjectionRole =
  | "working"
  | "saved"
  | "applied"
  | "runtime_observation";

export type EnvironmentChangeSliceKind = "unsaved" | "pending" | "drift";

export type EnvironmentChangeProvenance = {
  baseline: { role: EnvironmentProjectionRole; token: string };
  target: { role: EnvironmentProjectionRole; token: string };
};

export type EnvironmentProjectedLifecycleChange = {
  id: ChangeId;
  owner: { node: EnvironmentNodeIdentity };
  kind: EnvironmentNodeLifecycle;
  resettable: boolean;
};

type EnvironmentDiscardTarget =
  | { target: "working" }
  | {
      target: "saved";
      basis: EnvironmentSavedStateDiscardCommand["basis"];
    };

export type EnvironmentSettingDiscardPlan = {
  kind: "restore_setting";
  owner: EnvironmentProjectedChangeOwner;
  config: EnvironmentNodeConfigByType[EnvironmentNodeIdentity["type"]];
} & EnvironmentDiscardTarget;

export type EnvironmentProjectedSettingChange = {
  id: ChangeId;
  owner: EnvironmentProjectedChangeOwner;
  label: string;
  kind: "add" | "update" | "remove" | "drift";
  baselineValue: DisplayValue;
  targetValue: DisplayValue;
  baselineSource: {
    role: EnvironmentProjectionRole | "node_introduction";
    token: string;
  } | null;
  resettable: boolean;
  discardPlan: EnvironmentSettingDiscardPlan | null;
};

export type EnvironmentNodeDiscardPlan =
  | ({
      kind: "restore";
      node: EnvironmentNodeIdentity;
    } & EnvironmentDiscardTarget)
  | ({
      kind: "delete";
      node: EnvironmentNodeIdentity;
    } & EnvironmentDiscardTarget);

export type EnvironmentProjectedNodeChange = {
  id: ChangeId;
  node: EnvironmentNodeIdentity;
  presence: {
    baseline: "present" | "absent";
    target: "present" | "absent";
  };
  lifecycle: EnvironmentProjectedLifecycleChange;
  settings: EnvironmentProjectedSettingChange[];
  discardPlan: EnvironmentNodeDiscardPlan | null;
};

export type EnvironmentChangeSlice = {
  provenance: EnvironmentChangeProvenance;
  groups: EnvironmentProjectedNodeChange[];
  lifecycleCount: number;
  settingCount: number;
  totalCount: number;
  discardPlans: {
    nodes: EnvironmentNodeDiscardPlan[];
    settings: EnvironmentSettingDiscardPlan[];
  };
};

export type EnvironmentChangeSet = {
  unsaved: EnvironmentChangeSlice;
  pending: EnvironmentChangeSlice;
  drift: EnvironmentChangeSlice;
  discardAllPlan: import("@ployz/sdk/config").ReviewAggregateDiscardPlan;
};

export type EnvironmentWorkingComparison<T> =
  | { role: "saved"; value: T }
  | { role: "node_introduction"; value: T }
  | null;

export const resolveEnvironmentWorkingComparison = resolveWorkingComparison;

function presentSlice(slice: ReviewChangeSlice): EnvironmentChangeSlice {
  return {
    ...slice,
    groups: slice.groups.map((group) => ({
      ...group,
      lifecycle: { ...group.lifecycle, resettable: group.discardPlan !== null },
      settings: group.settings.map((setting) => {
        const display = presentSettingChange(group.node.type, setting.owner.setting,
          setting.baselineValue, setting.targetValue);
        return {
          ...setting,
          resettable: setting.discardPlan !== null,
          label: setting.label ?? display.label,
          baselineValue: setting.label === null ? display.currentValue : asString(setting.baselineValue),
          targetValue: setting.label === null ? display.newValue : asString(setting.targetValue),
        };
      }),
    })),
  };
}

/** Rust owns projection and restore decisions; this adapter formats the canvas. */
export function buildEnvironmentChangeSet(input: EnvironmentChangeSetProjectionInput): EnvironmentChangeSet {
  const result = projectEnvironmentChanges(input);
  return {
    unsaved: presentSlice(result.unsaved),
    pending: presentSlice(result.pending),
    drift: presentSlice(result.drift),
    discardAllPlan: result.discardAllPlan,
  };
}
