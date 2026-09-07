import { describe, expect, it } from "vitest";
import {
  buildEnvironmentChangeSet,
  resolveEnvironmentWorkingComparison,
  type EnvironmentChangeSetProjectionInput,
  type EnvironmentNodeConfigByType,
  type EnvironmentNodeIdentity,
  type EnvironmentNodeIntroductionProjection,
  type EnvironmentNodeProjection,
} from "#/modules/environment-design/environment-change-set";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { VariableGroupConfig } from "#/modules/environment-design/variable-group-config";
import { namedVolumeConfig } from "#/modules/environment-design/volume-config";

function serviceConfig(
  overrides: Partial<ServiceDeploymentConfig> = {},
): ServiceDeploymentConfig {
  return {
    version: 2,
    name: "api",
    source: {
      version: 1,
      type: "image",
      image: "docker.io/library/nginx:stable",
      autoUpdate: { type: "off" },
      credentials: { type: "none" },
    },
    preDeployCommand: null,
    startCommand: null,
    healthcheck: { type: "none" },
    restartPolicy: "unless-stopped",
    maxRetries: 10,
    cron: null,
    replicas: 1,
    cpuLimit: null,
    memLimit: null,
    privateDns: "api",
    routes: [],
    managedHostname: null,
    build: { builder: "auto", dockerfilePath: null, watchPaths: [] },
    env: {},
    mounts: [],
    ...overrides,
  };
}

type NodeType = EnvironmentNodeIdentity["type"];

type PresenceCase<TNodeType extends NodeType = NodeType> = {
  node: EnvironmentNodeIdentity & { type: TNodeType };
  introduced: EnvironmentNodeConfigByType[TNodeType];
  updated: EnvironmentNodeConfigByType[TNodeType];
  setting: string;
  baselineValue: string;
  targetValue: string;
  settingResettable: boolean;
};

const variableGroupConfig: VariableGroupConfig = {
  version: 1,
  name: "Shared",
  variables: [],
};
const volumeConfig = namedVolumeConfig("data");

const servicePresenceCase = {
  node: { type: "service", id: "service-1" },
  introduced: serviceConfig(),
  updated: serviceConfig({ replicas: 2 }),
  setting: "replicas",
  baselineValue: "1",
  targetValue: "2",
  settingResettable: true,
} satisfies PresenceCase<"service">;
const variableGroupPresenceCase = {
  node: {
    type: "variable_group",
    id: "variable-group-1",
  },
  introduced: variableGroupConfig,
  updated: { ...variableGroupConfig, name: "Shared Next" },
  setting: "name",
  baselineValue: "Shared",
  targetValue: "Shared Next",
  settingResettable: false,
} satisfies PresenceCase<"variable_group">;
const volumePresenceCase = {
  node: { type: "volume", id: "volume-1" },
  introduced: volumeConfig,
  updated: namedVolumeConfig("data-next"),
  setting: "name",
  baselineValue: "data",
  targetValue: "data-next",
  settingResettable: false,
} satisfies PresenceCase<"volume">;

const presenceCases = [
  servicePresenceCase,
  variableGroupPresenceCase,
  volumePresenceCase,
] satisfies PresenceCase[];

const presenceTransitions = [
  { baselinePresent: false, targetPresent: false, lifecycle: null },
  { baselinePresent: false, targetPresent: true, lifecycle: "create" },
  { baselinePresent: true, targetPresent: false, lifecycle: "delete" },
  { baselinePresent: true, targetPresent: true, lifecycle: null },
] as const;

const sliceCases = ["unsaved", "pending", "drift"] as const;
const savedStateSnapshotId = "00000000-0000-4000-8000-000000000001";

function project<TNodeType extends NodeType>(
  node: EnvironmentNodeIdentity & { type: TNodeType },
  config: EnvironmentNodeConfigByType[TNodeType] | null,
): EnvironmentNodeProjection {
  return { node, config } as EnvironmentNodeProjection;
}

function introduce<TNodeType extends NodeType>(
  node: EnvironmentNodeIdentity & { type: TNodeType },
  config: EnvironmentNodeConfigByType[TNodeType],
): EnvironmentNodeIntroductionProjection {
  return { node, config } as EnvironmentNodeIntroductionProjection;
}

function identity(node: EnvironmentNodeIdentity) {
  return { type: node.type, id: node.id };
}

function projectionInput<TNodeType extends NodeType>(input: {
  testCase: PresenceCase<TNodeType>;
  working: EnvironmentNodeConfigByType[TNodeType] | null;
  saved: EnvironmentNodeConfigByType[TNodeType] | null;
  applied: EnvironmentNodeConfigByType[TNodeType] | null;
  runtimeObserved: EnvironmentNodeConfigByType[TNodeType] | null;
}): EnvironmentChangeSetProjectionInput {
  const { node, introduced } = input.testCase;
  return {
    working: { token: "working-1", nodes: [project(node, input.working)] },
    saved: {
      kind: "saved_revision",
      savedStateSnapshotId,
      token: "saved-1",
      nodes: [project(node, input.saved)],
    },
    applied: { token: "applied-1", nodes: [project(node, input.applied)] },
    nodeIntroductions: {
      token: "introductions-1",
      nodes: [introduce(node, introduced)],
    },
    runtimeObserved: {
      token: "runtime-1",
      nodes: [project(node, input.runtimeObserved)],
    },
  };
}

function presenceTransitionChangeSet<TNodeType extends NodeType>(input: {
  testCase: PresenceCase<TNodeType>;
  slice: (typeof sliceCases)[number];
  baselinePresent: boolean;
  targetPresent: boolean;
}) {
  const baseline = input.baselinePresent ? input.testCase.introduced : null;
  const target = input.targetPresent ? input.testCase.introduced : null;

  switch (input.slice) {
    case "unsaved":
      return buildEnvironmentChangeSet(
        projectionInput({
          testCase: input.testCase,
          working: target,
          saved: baseline,
          applied: null,
          runtimeObserved: null,
        }),
      );
    case "pending":
      return buildEnvironmentChangeSet(
        projectionInput({
          testCase: input.testCase,
          working: target,
          saved: target,
          applied: baseline,
          runtimeObserved: baseline,
        }),
      );
    case "drift":
      return buildEnvironmentChangeSet(
        projectionInput({
          testCase: input.testCase,
          working: baseline,
          saved: baseline,
          applied: baseline,
          runtimeObserved: target,
        }),
      );
  }
}

describe("environment change set", () => {
  it("resolves the one canonical Working comparison source", () => {
    expect(
      resolveEnvironmentWorkingComparison({
        saved: "saved",
        applied: "applied",
        introduction: "introduced",
      }),
    ).toEqual({ role: "saved", value: "saved" });
    expect(
      resolveEnvironmentWorkingComparison({
        saved: null,
        applied: "applied",
        introduction: "introduced",
      }),
    ).toBeNull();
    expect(
      resolveEnvironmentWorkingComparison({
        saved: null,
        applied: null,
        introduction: "introduced",
      }),
    ).toEqual({ role: "node_introduction", value: "introduced" });
  });

  it("separates a Working Service edit from Saved, pending, and drift state", () => {
    const node = { type: "service" as const, id: "service-1", name: "api" };
    const nodeIdentity = identity(node);
    const savedConfig = serviceConfig();
    const workingConfig = serviceConfig({ replicas: 2 });

    const changeSet = buildEnvironmentChangeSet({
      working: {
        token: "working-2",
        nodes: [{ node, config: workingConfig }],
      },
      saved: {
        kind: "saved_revision",
        savedStateSnapshotId,
        token: "saved-1",
        nodes: [{ node, config: savedConfig }],
      },
      applied: {
        token: "applied-1",
        nodes: [{ node, config: savedConfig }],
      },
      nodeIntroductions: {
        token: "introductions-1",
        nodes: [{ node, config: savedConfig }],
      },
      runtimeObserved: {
        token: "runtime-1",
        nodes: [{ node, config: savedConfig }],
      },
    });

    expect(changeSet.unsaved).toEqual({
      provenance: {
        baseline: { role: "saved", token: "saved-1" },
        target: { role: "working", token: "working-2" },
      },
      groups: [
        {
          id: "service:service-1",
          node: nodeIdentity,
          presence: { baseline: "present", target: "present" },
          lifecycle: {
            id: "service:service-1:lifecycle",
            owner: { node: nodeIdentity },
            kind: "update",
            resettable: true,
          },
          settings: [
            {
              id: "service:service-1:replicas",
              owner: { node: nodeIdentity, setting: "replicas" },
              label: "Replicas",
              kind: "update",
              baselineValue: "1",
              targetValue: "2",
              baselineSource: { role: "saved", token: "saved-1" },
              resettable: true,
              discardPlan: {
                kind: "restore_setting",
                target: "working",
                owner: { node: nodeIdentity, setting: "replicas" },
                config: savedConfig,
              },
            },
          ],
          discardPlan: {
            kind: "restore",
            target: "working",
            node: nodeIdentity,
          },
        },
      ],
      lifecycleCount: 0,
      settingCount: 1,
      totalCount: 1,
      discardPlans: {
        nodes: [
          {
            kind: "restore",
            target: "working",
            node: nodeIdentity,
          },
        ],
        settings: [
          {
            kind: "restore_setting",
            target: "working",
            owner: { node: nodeIdentity, setting: "replicas" },
            config: savedConfig,
          },
        ],
      },
    });
    expect(changeSet.pending.totalCount).toBe(0);
    expect(changeSet.drift.totalCount).toBe(0);
  });

  it("does not construct a discard command for a non-discardable Service env row", () => {
    const node = servicePresenceCase.node;
    const saved = serviceConfig({
      env: { API_KEY: { kind: "literal", value: "saved" } },
    });
    const working = serviceConfig({
      env: { API_KEY: { kind: "literal", value: "working" } },
    });
    const changeSet = buildEnvironmentChangeSet({
      working: { token: "working-1", nodes: [project(node, working)] },
      saved: {
        kind: "saved_revision",
        savedStateSnapshotId,
        token: "saved-1",
        nodes: [project(node, saved)],
      },
      applied: { token: "applied-1", nodes: [project(node, saved)] },
      nodeIntroductions: { token: "introductions-1", nodes: [] },
      runtimeObserved: null,
    });

    expect(changeSet.unsaved.groups[0]?.settings[0]).toMatchObject({
      owner: { setting: "env.API_KEY" },
      resettable: false,
      discardPlan: null,
    });
    expect(changeSet.unsaved.discardPlans.settings).toEqual([]);
  });

  it.each(presenceCases)(
    "owns $node.type creation and introduction edits in the unsaved slice",
    (testCase) => {
      const changeSet = buildEnvironmentChangeSet(
        projectionInput({
          testCase,
          working: testCase.updated,
          saved: null,
          applied: null,
          runtimeObserved: null,
        }),
      );

      expect(changeSet.unsaved.groups).toMatchObject([
        {
          id: `${testCase.node.type}:${testCase.node.id}`,
          node: identity(testCase.node),
          presence: { baseline: "absent", target: "present" },
          lifecycle: {
            id: `${testCase.node.type}:${testCase.node.id}:lifecycle`,
            owner: { node: identity(testCase.node) },
            kind: "create",
            resettable: true,
          },
          settings: [
            {
              id: `${testCase.node.type}:${testCase.node.id}:${testCase.setting}`,
              owner: {
                node: identity(testCase.node),
                setting: testCase.setting,
              },
              kind: "update",
              baselineValue: testCase.baselineValue,
              targetValue: testCase.targetValue,
              baselineSource: {
                role: "node_introduction",
                token: "introductions-1",
              },
              resettable: testCase.settingResettable,
            },
          ],
          discardPlan: {
            kind: "delete",
            target: "working",
            node: identity(testCase.node),
          },
        },
      ]);
      expect(changeSet.unsaved).toMatchObject({
        lifecycleCount: 1,
        settingCount: 1,
        totalCount: 2,
      });
      expect(changeSet.unsaved.groups[0]?.settings[0]?.discardPlan).toEqual(
        testCase.settingResettable
          ? {
              kind: "restore_setting",
              target: "working",
              owner: {
                node: identity(testCase.node),
                setting: testCase.setting,
              },
              config: testCase.introduced,
            }
          : null,
      );
    },
  );

  it.each(presenceCases)(
    "keeps a failed $node.type update pending from Applied to Saved",
    (testCase) => {
      const changeSet = buildEnvironmentChangeSet(
        projectionInput({
          testCase,
          working: testCase.updated,
          saved: testCase.updated,
          applied: testCase.introduced,
          runtimeObserved: testCase.introduced,
        }),
      );

      expect(changeSet.unsaved.totalCount).toBe(0);
      expect(changeSet.pending.groups).toMatchObject([
        {
          presence: { baseline: "present", target: "present" },
          lifecycle: { kind: "update" },
          settings: [
            {
              id: `${testCase.node.type}:${testCase.node.id}:${testCase.setting}`,
              owner: {
                node: identity(testCase.node),
                setting: testCase.setting,
              },
              kind: "update",
              baselineValue: testCase.baselineValue,
              targetValue: testCase.targetValue,
              resettable: testCase.settingResettable,
            },
          ],
          discardPlan: {
            kind: "restore",
            target: "saved",
            basis: {
              kind: "saved_revision",
              savedStateSnapshotId,
            },
            node: identity(testCase.node),
          },
        },
      ]);
      expect(changeSet.pending.totalCount).toBe(1);
    },
  );

  it.each(presenceCases)(
    "keeps a failed $node.type removal pending until Applied confirms absence",
    (testCase) => {
      const changeSet = buildEnvironmentChangeSet(
        projectionInput({
          testCase,
          working: null,
          saved: null,
          applied: testCase.introduced,
          runtimeObserved: testCase.introduced,
        }),
      );

      expect(changeSet.pending.groups).toEqual([
        {
          id: `${testCase.node.type}:${testCase.node.id}`,
          node: identity(testCase.node),
          presence: { baseline: "present", target: "absent" },
          lifecycle: {
            id: `${testCase.node.type}:${testCase.node.id}:lifecycle`,
            owner: { node: identity(testCase.node) },
            kind: "delete",
            resettable: true,
          },
          settings: [],
          discardPlan: {
            kind: "restore",
            target: "saved",
            basis: {
              kind: "saved_revision",
              savedStateSnapshotId,
            },
            node: identity(testCase.node),
          },
        },
      ]);
      expect(changeSet.pending).toMatchObject({
        lifecycleCount: 1,
        settingCount: 0,
        totalCount: 1,
      });
    },
  );

  it.each(presenceCases)(
    "reports an observed $node.type update only as non-resettable drift",
    (testCase) => {
      const changeSet = buildEnvironmentChangeSet(
        projectionInput({
          testCase,
          working: testCase.introduced,
          saved: testCase.introduced,
          applied: testCase.introduced,
          runtimeObserved: testCase.updated,
        }),
      );

      expect(changeSet.unsaved.totalCount).toBe(0);
      expect(changeSet.pending.totalCount).toBe(0);
      expect(changeSet.drift).toMatchObject({
        provenance: {
          baseline: { role: "applied", token: "applied-1" },
          target: { role: "runtime_observation", token: "runtime-1" },
        },
        groups: [
          {
            presence: { baseline: "present", target: "present" },
            lifecycle: { kind: "update" },
            settings: [
              {
                id: `${testCase.node.type}:${testCase.node.id}:${testCase.setting}`,
                owner: {
                  node: identity(testCase.node),
                  setting: testCase.setting,
                },
                kind: "drift",
                baselineValue: testCase.baselineValue,
                targetValue: testCase.targetValue,
                resettable: false,
              },
            ],
            discardPlan: null,
          },
        ],
        lifecycleCount: 0,
        settingCount: 1,
        totalCount: 1,
        discardPlans: { nodes: [], settings: [] },
      });
    },
  );

  it.each(
    presenceCases.flatMap((testCase) =>
      sliceCases.flatMap((slice) =>
        presenceTransitions.map((transition) => ({
          testCase,
          slice,
          ...transition,
        })),
      ),
    ),
  )(
    "$slice presence transition for $testCase.node.type: $baselinePresent → $targetPresent",
    ({ testCase, slice, baselinePresent, targetPresent, lifecycle }) => {
      const changeSet = presenceTransitionChangeSet({
        testCase,
        slice,
        baselinePresent,
        targetPresent,
      });
      const group = changeSet[slice].groups[0];

      expect(group?.lifecycle?.kind ?? null).toBe(lifecycle);
      expect(changeSet[slice].lifecycleCount).toBe(lifecycle ? 1 : 0);
      expect(group?.lifecycle?.resettable ?? false).toBe(
        lifecycle ? slice !== "drift" : false,
      );
    },
  );

  it("does not claim Variable Group-derived environment values as Service-owned settings", () => {
    const node = { type: "service" as const, id: "service-1", name: "api" };
    const source = {
      kind: "variable_group" as const,
      resourceId: "11111111-1111-4111-8111-111111111111",
      resourceName: "Shared",
      variableGroupId: "22222222-2222-4222-8222-222222222222",
      key: "DATABASE_URL",
    };
    const savedConfig = serviceConfig({
      env: {
        DATABASE_URL: { kind: "literal", value: "old", source },
      },
    });
    const workingConfig = serviceConfig({
      env: {
        DATABASE_URL: { kind: "literal", value: "new", source },
      },
    });

    const changeSet = buildEnvironmentChangeSet({
      working: {
        token: "working-2",
        nodes: [project(node, workingConfig)],
      },
      saved: {
        kind: "saved_revision",
        savedStateSnapshotId,
        token: "saved-1",
        nodes: [project(node, savedConfig)],
      },
      applied: { token: "applied-1", nodes: [project(node, savedConfig)] },
      nodeIntroductions: {
        token: "introductions-1",
        nodes: [introduce(node, savedConfig)],
      },
      runtimeObserved: {
        token: "runtime-1",
        nodes: [project(node, savedConfig)],
      },
    });

    expect(changeSet.unsaved.groups).toEqual([]);
    expect(changeSet.unsaved.totalCount).toBe(0);
  });

  it("does not use Applied as the Unsaved baseline when Saved is absent", () => {
    const testCase = servicePresenceCase;
    const appliedConfig = serviceConfig({ replicas: 2 });
    const workingConfig = serviceConfig({ replicas: 3 });
    const changeSet = buildEnvironmentChangeSet(
      projectionInput({
        testCase,
        working: workingConfig,
        saved: null,
        applied: appliedConfig,
        runtimeObserved: appliedConfig,
      }),
    );

    expect(changeSet.unsaved.groups[0]).toMatchObject({
      lifecycle: { kind: "create" },
    });
    expect(
      changeSet.unsaved.groups[0]?.settings.every(
        (setting) => setting.baselineSource?.role !== "applied",
      ),
    ).toBe(true);
  });

  it("does not use Introduction as the pending baseline after Saved exists", () => {
    const changeSet = buildEnvironmentChangeSet(
      projectionInput({
        testCase: servicePresenceCase,
        working: servicePresenceCase.updated,
        saved: servicePresenceCase.updated,
        applied: null,
        runtimeObserved: null,
      }),
    );

    expect(changeSet.pending.groups[0]).toMatchObject({
      lifecycle: { kind: "create" },
      settings: [],
    });
    expect(changeSet.pending.settingCount).toBe(0);
  });

  it("does not use Introduction as the drift baseline", () => {
    const changeSet = buildEnvironmentChangeSet(
      projectionInput({
        testCase: servicePresenceCase,
        working: servicePresenceCase.updated,
        saved: servicePresenceCase.updated,
        applied: null,
        runtimeObserved: servicePresenceCase.updated,
      }),
    );

    expect(changeSet.drift.groups[0]).toMatchObject({
      lifecycle: { kind: "create" },
      settings: [],
    });
    expect(changeSet.drift.settingCount).toBe(0);
  });

  it("returns a deterministic serializable value without mutating projections", () => {
    const testCase = variableGroupPresenceCase;
    const input = projectionInput({
      testCase,
      working: testCase.updated,
      saved: testCase.introduced,
      applied: testCase.introduced,
      runtimeObserved: testCase.updated,
    });
    const before = structuredClone(input);

    const first = buildEnvironmentChangeSet(input);
    const second = buildEnvironmentChangeSet(input);

    expect(input).toEqual(before);
    expect(first).toEqual(second);
    expect(JSON.parse(JSON.stringify(first))).toEqual(first);
  });

  it("keeps a setting identity stable as it moves between provenance slices", () => {
    const testCase = servicePresenceCase;
    const changeSet = buildEnvironmentChangeSet(
      projectionInput({
        testCase,
        working: serviceConfig({ replicas: 3 }),
        saved: serviceConfig({ replicas: 2 }),
        applied: serviceConfig({ replicas: 1 }),
        runtimeObserved: serviceConfig({ replicas: 1 }),
      }),
    );

    expect(changeSet.unsaved.groups[0]?.settings[0]?.id).toBe(
      changeSet.pending.groups[0]?.settings[0]?.id,
    );
  });

  it("keeps machine ownership stable when presentation names change", () => {
    const savedNode = servicePresenceCase.node;
    const workingNode = { ...savedNode, name: "api-next" };
    const savedConfig = serviceConfig();
    const workingConfig = serviceConfig({ replicas: 2 });
    const changeSet = buildEnvironmentChangeSet({
      working: {
        token: "working-2",
        nodes: [project(workingNode, workingConfig)],
      },
      saved: {
        kind: "saved_revision",
        savedStateSnapshotId,
        token: "saved-1",
        nodes: [project(savedNode, savedConfig)],
      },
      applied: {
        token: "applied-1",
        nodes: [project(savedNode, savedConfig)],
      },
      nodeIntroductions: {
        token: "introductions-1",
        nodes: [introduce(savedNode, savedConfig)],
      },
      runtimeObserved: {
        token: "runtime-1",
        nodes: [project(savedNode, savedConfig)],
      },
    });

    expect(changeSet.unsaved.groups[0]?.settings[0]?.owner).toEqual({
      node: { type: "service", id: "service-1" },
      setting: "replicas",
    });
    expect(changeSet.unsaved.groups[0]?.node).toEqual({
      type: "service",
      id: "service-1",
    });
  });
});
