import { describe, expect, it } from "vitest";
import {
  buildCanvasEnvironmentChangeState,
  buildCanvasRuntimeObservations,
  type CanvasDeploymentEvidence,
} from "#/modules/environment-design/canvas-environment-change-state";
import type {
  EnvironmentNodeProjection,
  EnvironmentSavedStateProjection,
  EnvironmentStateProjection,
} from "#/modules/environment-design/environment-change-set";
import { namedVolumeConfig } from "#/modules/environment-design/volume-config";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";

const node = { type: "service" as const, id: "service-1" };

function serviceConfig(replicas: number): ServiceDeploymentConfig {
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
    replicas,
    cpuLimit: null,
    memLimit: null,
    privateDns: "api",
    routes: [],
    managedHostname: null,
    build: { builder: "auto", dockerfilePath: null, watchPaths: [] },
    env: {},
    mounts: [],
    variableGroupAttachments: [],
  };
}

function state(token: string, config: ServiceDeploymentConfig) {
  return {
    token,
    nodes: [{ node, config } satisfies EnvironmentNodeProjection],
  } satisfies EnvironmentStateProjection;
}

function savedState(
  token: string,
  config: ServiceDeploymentConfig,
): EnvironmentSavedStateProjection {
  return {
    ...state(token, config),
    kind: "saved_revision",
    savedStateSnapshotId: "00000000-0000-4000-8000-000000000001",
  };
}

describe("canvas Environment Change Set seam", () => {
  it("clears pending only for Services advanced in folded Applied State", () => {
    const database = { type: "service" as const, id: "database" };
    const worker = { type: "service" as const, id: "worker" };
    const saved = {
      kind: "saved_revision",
      savedStateSnapshotId: "00000000-0000-4000-8000-000000000001",
      token: "saved",
      nodes: [
        { node: database, config: serviceConfig(2) },
        { node: worker, config: serviceConfig(2) },
      ],
    } satisfies EnvironmentSavedStateProjection;
    const applied = {
      token: "applied:database:new|worker:old",
      nodes: [
        { node: database, config: serviceConfig(2) },
        { node: worker, config: serviceConfig(1) },
      ],
    } satisfies EnvironmentStateProjection;

    const canvasState = buildCanvasEnvironmentChangeState({
      working: saved,
      saved,
      applied,
      nodeIntroductions: { token: "none", nodes: [] },
      runtimeObserved: applied,
      deploymentEvidence: null,
      nodes: [
        { node: database, name: "database", summaryLabel: "database" },
        { node: worker, name: "worker", summaryLabel: "worker" },
      ],
    });

    expect(canvasState.slices.pending.groups).toMatchObject([
      {
        nodeId: "worker",
        rows: [{ path: "replicas", currentValue: "1", newValue: "2" }],
      },
    ]);
  });

  it("keeps a queued target as evidence while deriving unsaved and pending from explicit state", () => {
    const deploymentEvidence = {
      id: "deployment-1",
      status: "queued",
      token: "attempt-4",
      nodes: [{ node, config: serviceConfig(4) }],
    } satisfies CanvasDeploymentEvidence;

    const canvasState = buildCanvasEnvironmentChangeState({
      working: state("working-3", serviceConfig(3)),
      saved: savedState("saved-2", serviceConfig(2)),
      applied: state("applied-1", serviceConfig(1)),
      nodeIntroductions: {
        token: "introductions-1",
        nodes: [{ node, config: serviceConfig(1) }],
      },
      runtimeObserved: state("runtime-1", serviceConfig(1)),
      deploymentEvidence,
      nodes: [{ node, name: "api", summaryLabel: "api" }],
    });

    expect(canvasState.deploymentEvidence).toBe(deploymentEvidence);
    expect(canvasState.slices.unsaved).toMatchObject({
      totalCount: 1,
      groups: [
        {
          nodeType: "service",
          nodeId: "service-1",
          nodeName: "api",
          rows: [
            {
              path: "replicas",
              currentValue: "2",
              newValue: "3",
            },
          ],
        },
      ],
    });
    expect(canvasState.slices.pending).toMatchObject({
      totalCount: 1,
      groups: [
        {
          rows: [
            {
              path: "replicas",
              currentValue: "1",
              newValue: "2",
            },
          ],
        },
      ],
    });
    expect(canvasState.slices.drift.totalCount).toBe(0);
  });

  it("shows neither runtime drift nor a deploy affordance without a connection", () => {
    const canvasState = buildCanvasEnvironmentChangeState({
      working: state("working-2", serviceConfig(2)),
      saved: savedState("saved-2", serviceConfig(2)),
      applied: state("applied-1", serviceConfig(1)),
      nodeIntroductions: {
        token: "introductions-1",
        nodes: [{ node, config: serviceConfig(1) }],
      },
      runtimeObserved: null,
      deploymentEvidence: null,
      nodes: [{ node, name: "api", summaryLabel: "api" }],
    });

    expect(canvasState.canDeploy).toBe(false);
    expect(canvasState.slices.drift).toMatchObject({
      groups: [],
      lifecycleCount: 0,
      settingCount: 0,
      totalCount: 0,
    });
    expect(canvasState.slices.pending.totalCount).toBe(1);
  });

  it("presents runtime observations as canonical non-discardable drift", () => {
    const applied = state("applied-2", serviceConfig(2));
    const canvasState = buildCanvasEnvironmentChangeState({
      working: applied,
      saved: savedState("saved-2", serviceConfig(2)),
      applied,
      nodeIntroductions: {
        token: "introductions-1",
        nodes: [{ node, config: serviceConfig(1) }],
      },
      runtimeObserved: applied,
      runtimeObservations: {
        token: "runtime-3",
        settings: [
          {
            node,
            setting: "runtime.replicas",
            label: "Replicas",
            appliedValue: "2",
            observedValue: "3",
          },
        ],
      },
      deploymentEvidence: null,
      nodes: [{ node, name: "api", summaryLabel: "api" }],
    });

    expect(canvasState.slices.drift).toMatchObject({
      totalCount: 1,
      groups: [
        {
          canDiscard: false,
          rows: [
            {
              path: "runtime.replicas",
              currentValue: "3",
              newValue: "2",
              canDiscard: false,
            },
          ],
        },
      ],
    });
  });

  it("adapts observed runtime replicas against Applied config", () => {
    const observations = buildCanvasRuntimeObservations({
      environmentNamespace: "production",
      applied: state("applied-2", serviceConfig(2)),
      runtimeServices: [
        {
          id: "production:api",
          namespaceId: "production",
          serviceId: "api",
          activeRevisionId: "revision-1",
          routeCount: 0,
          instanceCount: 3,
          readyInstanceCount: 3,
          bindings: [],
          updatedAt: "2026-07-18T12:00:00.000Z",
        },
      ],
      autoDomain: null,
      appliedRevisionByNodeId: new Map([["service-1", "revision-applied"]]),
    });

    expect(observations.settings).toContainEqual({
      node,
      setting: "runtime.replicas",
      label: "Replicas",
      appliedValue: "2",
      observedValue: "3",
    });
    expect(observations.settings).toContainEqual({
      node,
      setting: "runtime.revision",
      label: "Runtime revision",
      appliedValue: "revision-applied",
      observedValue: "revision-1",
    });
  });

  it("reports runtime presence changes instead of synthesizing replica drift", () => {
    const observations = buildCanvasRuntimeObservations({
      environmentNamespace: "production",
      applied: state("applied-2", serviceConfig(2)),
      runtimeServices: [
        {
          id: "production:orphan",
          namespaceId: "production",
          serviceId: "orphan",
          activeRevisionId: "revision-orphan",
          routeCount: 0,
          instanceCount: 1,
          readyInstanceCount: 1,
          bindings: [],
          updatedAt: "2026-07-18T12:00:00.000Z",
        },
      ],
      autoDomain: null,
    });
    const canvasState = buildCanvasEnvironmentChangeState({
      working: state("working-2", serviceConfig(2)),
      saved: savedState("saved-2", serviceConfig(2)),
      applied: state("applied-2", serviceConfig(2)),
      nodeIntroductions: { token: "none", nodes: [] },
      runtimeObserved: state("runtime", serviceConfig(2)),
      runtimeObservations: observations,
      deploymentEvidence: null,
      nodes: [{ node, name: "api", summaryLabel: "api" }],
    });

    expect(canvasState.slices.drift.groups).toMatchObject([
      {
        nodeId: "orphan",
        lifecycle: "create",
        rows: [],
      },
      {
        nodeId: "service-1",
        lifecycle: "delete",
        rows: [],
      },
    ]);
    expect(canvasState.slices.drift.totalCount).toBe(2);
  });

  it("keeps pending Discard plans available to the presentation adapter", () => {
    const canvasState = buildCanvasEnvironmentChangeState({
      working: state("working-2", serviceConfig(2)),
      saved: savedState("saved-2", serviceConfig(2)),
      applied: state("applied-1", serviceConfig(1)),
      nodeIntroductions: { token: "none", nodes: [] },
      runtimeObserved: state("runtime-1", serviceConfig(1)),
      deploymentEvidence: null,
      nodes: [{ node, name: "api", summaryLabel: "api" }],
    });

    expect(canvasState.slices.pending.groups[0]).toMatchObject({
      canDiscard: true,
      rows: [{ path: "replicas", canDiscard: true }],
      projectedChange: {
        settings: [{ discardPlan: { target: "saved" } }],
      },
    });
  });

  it("plans Discard All once against the final Applied baseline", () => {
    const canvasState = buildCanvasEnvironmentChangeState({
      working: state("working-3", serviceConfig(3)),
      saved: savedState("saved-2", serviceConfig(2)),
      applied: state("applied-1", serviceConfig(1)),
      nodeIntroductions: { token: "none", nodes: [] },
      runtimeObserved: state("runtime-1", serviceConfig(1)),
      deploymentEvidence: null,
      nodes: [{ node, name: "api", summaryLabel: "api" }],
    });

    expect(canvasState.discardAllPlan.nodes).toEqual([
      {
        node,
        working: {
          kind: "restore",
          target: "working",
          node,
        },
      },
    ]);
    expect(canvasState.discardAllPlan.savedCommand).toEqual({
      kind: "discard",
      basis: {
        kind: "saved_revision",
        savedStateSnapshotId: "00000000-0000-4000-8000-000000000001",
      },
      operations: [
        { kind: "node", nodeType: "service", nodeId: node.id },
      ],
    });
  });

  it("orders resource resets before Services that can reference them", () => {
    const serviceNode = { type: "service" as const, id: "service-1" };
    const volumeNode = { type: "volume" as const, id: "volume-1" };
    const volumeConfig = namedVolumeConfig("data");
    const state = buildCanvasEnvironmentChangeState({
      working: {
        token: "working",
        nodes: [
          { node: serviceNode, config: serviceConfig(2) },
          { node: volumeNode, config: namedVolumeConfig("next") },
        ],
      },
      saved: {
        kind: "saved_revision",
        savedStateSnapshotId: "00000000-0000-4000-8000-000000000001",
        token: "saved",
        nodes: [
          { node: serviceNode, config: serviceConfig(1) },
          { node: volumeNode, config: volumeConfig },
        ],
      },
      applied: { token: "none", nodes: [] },
      nodeIntroductions: { token: "none", nodes: [] },
      runtimeObserved: null,
      deploymentEvidence: null,
      nodes: [
        { node: serviceNode, name: "api", summaryLabel: "api" },
        { node: volumeNode, name: "data", summaryLabel: "data" },
      ],
    });

    expect(state.discardAllPlan.nodes.map((plan) => plan.node.type)).toEqual([
      "volume",
      "service",
    ]);
    expect(state.discardAllPlan.savedCommand?.operations).toEqual([
      { kind: "node", nodeType: "volume", nodeId: volumeNode.id },
      { kind: "node", nodeType: "service", nodeId: serviceNode.id },
    ]);
  });

  it("does not use Applied as an Unsaved comparison fallback", () => {
    const canvasState = buildCanvasEnvironmentChangeState({
      working: state("working-2", serviceConfig(2)),
      saved: {
        kind: "saved_revision",
        savedStateSnapshotId: "00000000-0000-4000-8000-000000000001",
        token: "saved-deleted",
        nodes: [],
      },
      applied: state("applied-1", serviceConfig(1)),
      nodeIntroductions: {
        token: "introduced",
        nodes: [{ node, config: serviceConfig(1) }],
      },
      runtimeObserved: state("runtime-1", serviceConfig(1)),
      deploymentEvidence: null,
      nodes: [{ node, name: "api", summaryLabel: "api" }],
    });

    const unsaved = canvasState.slices.unsaved.groups[0];
    expect(unsaved?.lifecycle).toBe("create");
    expect(
      unsaved?.projectedChange.settings.every(
        (setting) => setting.baselineSource?.role !== "applied",
      ),
    ).toBe(true);
    expect(canvasState.slices.unsaved.provenance.baseline.role).toBe("saved");
  });
});
