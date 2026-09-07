import {
  getEnvironmentResourceNodeConfigDiffRows,
  isEnvironmentResourceNodeType,
  type EnvironmentResourceNodeConfigByType,
} from "#/modules/environment-design/environment-resource-node";
import {
  getServiceDeploymentAttemptDiffRows,
} from "#/modules/services/service-deployment-diff/state";
import type { DiffRow } from "#/modules/services/service-deployment-diff/fields";
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
};

export type EnvironmentWorkingComparison<T> =
  | { role: "saved"; value: T }
  | { role: "node_introduction"; value: T }
  | null;

/**
 * The single policy for comparing Working fields. Saved wins when present;
 * once Applied exists, Saved absence is authoritative; Introduction is only a
 * reset source before either later fact exists.
 */
export function resolveEnvironmentWorkingComparison<T>(input: {
  saved: T | null;
  applied: T | null;
  introduction: T | null;
}): EnvironmentWorkingComparison<T> {
  if (input.saved !== null) return { role: "saved", value: input.saved };
  if (input.applied !== null) return null;
  return input.introduction === null
    ? null
    : { role: "node_introduction", value: input.introduction };
}

function projectionKey(node: EnvironmentNodeIdentity) {
  return `${node.type}:${node.id}`;
}

function projectionMap(
  projection: EnvironmentStateProjection | EnvironmentNodeIntroductionsProjection,
) {
  return new Map(
    projection.nodes.map((projectedNode) => [
      projectionKey(projectedNode.node),
      projectedNode,
    ]),
  );
}

function getConfigDiffRows(input: {
  node: EnvironmentNodeIdentity;
  current: EnvironmentNodeConfigByType[EnvironmentNodeIdentity["type"]];
  baseline: EnvironmentNodeConfigByType[EnvironmentNodeIdentity["type"]] | null;
}): DiffRow[] {
  if (input.node.type === "service") {
    // SAFETY: node.type is "service"; current/baseline stay the config union TypeScript cannot narrow.
    return getServiceDeploymentAttemptDiffRows({
      serviceId: input.node.id,
      target: input.current as ServiceDeploymentConfig,
      deployed: input.baseline as ServiceDeploymentConfig | null,
    });
  }

  if (isEnvironmentResourceNodeType(input.node.type)) {
    // SAFETY: node.type is a resource type; current/baseline stay the config union TypeScript cannot narrow.
    return getEnvironmentResourceNodeConfigDiffRows({
      nodeType: input.node.type,
      nodeId: input.node.id,
      current: input.current as EnvironmentResourceNodeConfigByType[typeof input.node.type],
      baseline: input.baseline as EnvironmentResourceNodeConfigByType[typeof input.node.type] | null,
    });
  }

  return [];
}

function getLifecycleKind(input: {
  baselinePresent: boolean;
  targetPresent: boolean;
}): "create" | "delete" | null {
  if (!input.baselinePresent && input.targetPresent) return "create";
  if (input.baselinePresent && !input.targetPresent) return "delete";
  return null;
}

function buildDiscardPlan(input: {
  resettable: boolean;
  discardTarget: EnvironmentDiscardTarget | null;
  node: EnvironmentNodeIdentity;
  baselineConfig: EnvironmentNodeConfigByType[EnvironmentNodeIdentity["type"]] | null;
}): EnvironmentNodeDiscardPlan | null {
  if (!input.resettable || !input.discardTarget) return null;

  if (input.baselineConfig) {
    return {
      kind: "restore",
      ...input.discardTarget,
      node: input.node,
    };
  }

  return {
    kind: "delete",
    ...input.discardTarget,
    node: input.node,
  };
}

function buildEnvironmentChangeSlice(input: {
  kind: EnvironmentChangeSliceKind;
  baselineRole: EnvironmentProjectionRole;
  baseline: EnvironmentStateProjection;
  targetRole: EnvironmentProjectionRole;
  target: EnvironmentStateProjection;
  comparisonByKey?: ReadonlyMap<
    string,
    {
      config: EnvironmentNodeConfigByType[EnvironmentNodeIdentity["type"]];
      source: NonNullable<EnvironmentProjectedSettingChange["baselineSource"]>;
    }
  >;
  discardTarget: EnvironmentDiscardTarget | null;
}): EnvironmentChangeSlice {
  const baselineByKey = projectionMap(input.baseline);
  const targetByKey = projectionMap(input.target);
  const keys = [
    ...new Set([
      ...baselineByKey.keys(),
      ...targetByKey.keys(),
    ]),
  ].sort((left, right) => left.localeCompare(right));
  const resettable = input.kind !== "drift";
  const groups: EnvironmentProjectedNodeChange[] = [];

  for (const key of keys) {
    const baseline = baselineByKey.get(key);
    const target = targetByKey.get(key);
    const node = target?.node ?? baseline?.node;
    if (!node) continue;
    const nodeIdentity: EnvironmentNodeIdentity = {
      type: node.type,
      id: node.id,
    };

    const baselineConfig = baseline?.config ?? null;
    const targetConfig = target?.config ?? null;
    const comparisonBaseline = input.comparisonByKey
      ? input.comparisonByKey.get(key) ?? null
      : baselineConfig
        ? {
            config: baselineConfig,
            source: {
              role: input.baselineRole,
              token: input.baseline.token,
            },
          }
        : null;
    const presenceLifecycleKind = getLifecycleKind({
      baselinePresent: baselineConfig != null,
      targetPresent: targetConfig != null,
    });
    const rows = targetConfig && comparisonBaseline
      ? getConfigDiffRows({
          node,
          current: targetConfig,
          baseline: comparisonBaseline.config,
        }).filter((row) => row.path !== "node" && !row.derivedFrom)
      : [];
    const settings = rows.map<EnvironmentProjectedSettingChange>((row) => {
      const owner = { node: nodeIdentity, setting: row.path };
      const settingResettable =
        resettable && comparisonBaseline != null && row.canDiscard !== false;
      return {
        id: `${node.type}:${node.id}:${row.path}`,
        owner,
        label: row.label,
        kind: input.kind === "drift" ? "drift" : row.kind,
        baselineValue: row.currentValue,
        targetValue: row.newValue,
        baselineSource: comparisonBaseline?.source ?? null,
        resettable: settingResettable,
        discardPlan:
          settingResettable && input.discardTarget && comparisonBaseline
            ? {
                kind: "restore_setting",
                ...input.discardTarget,
                owner,
                config: comparisonBaseline.config,
              }
            : null,
      };
    });
    const lifecycleKind =
      presenceLifecycleKind ?? (settings.length > 0 ? "update" : "none");

    if (lifecycleKind === "none") continue;

    const lifecycle: EnvironmentProjectedLifecycleChange = {
      id: `${node.type}:${node.id}:lifecycle`,
      owner: { node: nodeIdentity },
      kind: lifecycleKind,
      resettable,
    };

    const discardPlan = buildDiscardPlan({
      resettable,
      discardTarget: input.discardTarget,
      node: nodeIdentity,
      baselineConfig,
    });
    groups.push({
      id: `${node.type}:${node.id}`,
      node: nodeIdentity,
      presence: {
        baseline: baselineConfig ? "present" : "absent",
        target: targetConfig ? "present" : "absent",
      },
      lifecycle,
      settings,
      discardPlan,
    });
  }

  const lifecycleCount = groups.filter(
    (group) =>
      group.lifecycle.kind === "create" || group.lifecycle.kind === "delete",
  ).length;
  const settingCount = groups.reduce(
    (count, group) => count + group.settings.length,
    0,
  );

  return {
    provenance: {
      baseline: { role: input.baselineRole, token: input.baseline.token },
      target: { role: input.targetRole, token: input.target.token },
    },
    groups,
    lifecycleCount,
    settingCount,
    totalCount: lifecycleCount + settingCount,
    discardPlans: {
      nodes: groups.flatMap((group) =>
        group.discardPlan ? [group.discardPlan] : [],
      ),
      settings: groups.flatMap((group) =>
        group.settings.flatMap((setting) =>
          setting.discardPlan ? [setting.discardPlan] : [],
        ),
      ),
    },
  };
}

/** Pure, serializable interface shared by Cloud and runtime contract fixtures. */
export function buildEnvironmentChangeSet(
  input: EnvironmentChangeSetProjectionInput,
): EnvironmentChangeSet {
  const drift = buildEnvironmentChangeSlice({
    kind: "drift",
    baselineRole: "applied",
    baseline: input.applied,
    targetRole: "runtime_observation",
    target: input.runtimeObserved ?? {
      token: "runtime:unavailable",
      nodes: input.applied.nodes,
    },
    discardTarget: null,
  });

  if (input.runtimeObserved && input.runtimeObservations) {
    const groupsByKey = new Map(
      drift.groups.map((group) => [projectionKey(group.node), group]),
    );
    for (const observation of input.runtimeObservations.settings) {
      const key = projectionKey(observation.node);
      const group = groupsByKey.get(key) ?? {
        id: key,
        node: observation.node,
        presence: { baseline: "present", target: "present" },
        lifecycle: {
          id: `${key}:lifecycle`,
          owner: { node: observation.node },
          kind: "update",
          resettable: false,
        },
        settings: [],
        discardPlan: null,
      } satisfies EnvironmentProjectedNodeChange;
      if (!groupsByKey.has(key)) {
        drift.groups.push(group);
        groupsByKey.set(key, group);
      }
      const id = `${key}:${observation.setting}`;
      if (group.settings.some((setting) => setting.id === id)) continue;
      group.settings.push({
        id,
        owner: { node: observation.node, setting: observation.setting },
        label: observation.label,
        kind: "drift",
        baselineValue: observation.appliedValue,
        targetValue: observation.observedValue,
        baselineSource: {
          role: "applied",
          token: input.applied.token,
        },
        resettable: false,
        discardPlan: null,
      });
    }
    for (const observation of input.runtimeObservations.presence ?? []) {
      const key = projectionKey(observation.node);
      const group = groupsByKey.get(key) ?? {
        id: key,
        node: observation.node,
        presence: {
          baseline: observation.applied,
          target: observation.observed,
        },
        lifecycle: {
          id: `${key}:lifecycle`,
          owner: { node: observation.node },
          kind:
            observation.applied === "present" ? "delete" : "create",
          resettable: false,
        },
        settings: [],
        discardPlan: null,
      } satisfies EnvironmentProjectedNodeChange;
      group.presence = {
        baseline: observation.applied,
        target: observation.observed,
      };
      group.lifecycle.kind =
        observation.applied === "present" ? "delete" : "create";
      if (!groupsByKey.has(key)) {
        drift.groups.push(group);
        groupsByKey.set(key, group);
      }
    }
    drift.groups.sort((left, right) => left.id.localeCompare(right.id));
    for (const group of drift.groups) {
      group.settings.sort((left, right) => left.id.localeCompare(right.id));
    }
    drift.lifecycleCount = drift.groups.filter(
      (group) =>
        group.lifecycle.kind === "create" ||
        group.lifecycle.kind === "delete",
    ).length;
    drift.settingCount = drift.groups.reduce(
      (count, group) => count + group.settings.length,
      0,
    );
    drift.totalCount = drift.lifecycleCount + drift.settingCount;
    drift.provenance.target.token = input.runtimeObservations.token;
  }

  const savedByKey = projectionMap(input.saved);
  const appliedByKey = projectionMap(input.applied);
  const introductionsByKey = projectionMap(input.nodeIntroductions);
  const workingComparisonByKey = new Map<
    string,
    {
      config: EnvironmentNodeConfigByType[EnvironmentNodeIdentity["type"]];
      source: NonNullable<
        EnvironmentProjectedSettingChange["baselineSource"]
      >;
    }
  >();
  for (const working of input.working.nodes) {
    const key = projectionKey(working.node);
    const comparison = resolveEnvironmentWorkingComparison({
      saved: savedByKey.get(key)?.config ?? null,
      applied: appliedByKey.get(key)?.config ?? null,
      introduction: introductionsByKey.get(key)?.config ?? null,
    });
    if (!comparison) continue;
    workingComparisonByKey.set(key, {
      config: comparison.value,
      source: {
        role: comparison.role,
        token:
          comparison.role === "saved"
            ? input.saved.token
            : input.nodeIntroductions.token,
      },
    });
  }

  return {
    unsaved: buildEnvironmentChangeSlice({
      kind: "unsaved",
      baselineRole: "saved",
      baseline: input.saved,
      targetRole: "working",
      target: input.working,
      comparisonByKey: workingComparisonByKey,
      discardTarget: { target: "working" },
    }),
    pending: buildEnvironmentChangeSlice({
      kind: "pending",
      baselineRole: "applied",
      baseline: input.applied,
      targetRole: "saved",
      target: input.saved,
      discardTarget:
        input.saved.kind === "saved_revision"
          ? {
              target: "saved",
              basis: {
                kind: "saved_revision",
                savedStateSnapshotId: input.saved.savedStateSnapshotId,
              },
            }
          : null,
    }),
    drift,
  };
}
