import { describe, expect, it } from "vitest";
import {
  buildCanvasEnvironmentChangeState,
  type CanvasDeploymentEvidence,
} from "#/modules/environment-design/canvas-environment-change-state";
import {
  buildEnvironmentChangeSet,
  type EnvironmentNodeProjection,
  type EnvironmentSavedStateProjection,
  type EnvironmentStateProjection,
} from "#/modules/environment-design/environment-change-set";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";

const node = { type: "service" as const, id: "api" };

function config(input: {
  replicas: number;
  image?: string;
  env?: Record<string, string>;
}): ServiceDeploymentConfig {
  return {
    version: 2,
    name: "api",
    source: {
      version: 1,
      type: "image",
      image: input.image ?? "registry.test/api:stable",
      autoUpdate: { type: "off" },
      credentials: { type: "none" },
    },
    preDeployCommand: null,
    startCommand: null,
    healthcheck: { type: "none" },
    restartPolicy: "unless-stopped",
    maxRetries: 10,
    cron: null,
    replicas: input.replicas,
    cpuLimit: null,
    memLimit: null,
    privateDns: "api",
    routes: [],
    managedHostname: null,
    build: { builder: "auto", dockerfilePath: null, watchPaths: [] },
    env: Object.fromEntries(
      Object.entries(input.env ?? {}).map(([key, value]) => [
        key,
        { kind: "literal" as const, value },
      ]),
    ),
    mounts: [],
    variableGroupAttachments: [],
  };
}

function state(
  token: string,
  serviceConfig: ServiceDeploymentConfig | null,
): EnvironmentStateProjection {
  return {
    token,
    nodes: [{ node, config: serviceConfig } satisfies EnvironmentNodeProjection],
  };
}

function savedState(
  token: string,
  serviceConfig: ServiceDeploymentConfig | null,
): EnvironmentSavedStateProjection {
  return {
    ...state(token, serviceConfig),
    kind: "saved_revision",
    savedStateSnapshotId: "00000000-0000-4000-8000-000000000001",
  };
}

function changeSet(input: {
  working: ServiceDeploymentConfig | null;
  saved: ServiceDeploymentConfig | null;
  applied: ServiceDeploymentConfig | null;
  runtime?: ServiceDeploymentConfig | null;
}) {
  return buildEnvironmentChangeSet({
    working: state("working", input.working),
    saved: savedState("saved", input.saved),
    applied: state("applied", input.applied),
    nodeIntroductions: { token: "introduction", nodes: [] },
    runtimeObserved: state("runtime", input.runtime ?? input.applied),
  });
}

function canvasState(input: {
  working: ServiceDeploymentConfig | null;
  saved: ServiceDeploymentConfig | null;
  applied: ServiceDeploymentConfig | null;
  runtime: ServiceDeploymentConfig | null;
  deploymentEvidence?: CanvasDeploymentEvidence | null;
}) {
  return buildCanvasEnvironmentChangeState({
    working: state("working", input.working),
    saved: savedState("saved", input.saved),
    applied: state("applied", input.applied),
    nodeIntroductions: { token: "introduction", nodes: [] },
    runtimeObserved: state("runtime", input.runtime),
    deploymentEvidence: input.deploymentEvidence ?? null,
    nodes: [{ node, name: "api", summaryLabel: "api" }],
  });
}

describe("Environment Change Set cross-layer acceptance matrix", () => {
  it("Save separates the published configuration from successor Working edits", () => {
    const applied = config({ replicas: 1, env: { FEATURE: "off" } });
    const saved = config({ replicas: 1, env: { FEATURE: "saved" } });
    const successorWorking = config({
      replicas: 1,
      env: { FEATURE: "unsaved-after-save" },
    });
    expect(
      changeSet({ working: saved, saved: applied, applied }).unsaved.totalCount,
    ).toBe(1);

    const afterSave = changeSet({
      working: successorWorking,
      saved,
      applied,
    });
    expect(afterSave.unsaved.totalCount).toBe(1);
    expect(afterSave.pending.totalCount).toBe(1);


  });

  it("unsaved exclusion keeps Working-only values out of Saved intent", () => {
    const saved = config({ replicas: 2 });
    const working = config({ replicas: 3 });
    const projected = changeSet({ working, saved, applied: saved });

    expect(projected.unsaved.groups[0]?.settings).toMatchObject([
      { owner: { setting: "replicas" }, baselineValue: "2", targetValue: "3" },
    ]);
    expect(projected.pending.totalCount).toBe(0);
  });

  it("manual failure leaves Saved work pending", () => {
    const applied = config({ replicas: 1 });
    const saved = config({ replicas: 2 });
    const projected = changeSet({ working: saved, saved, applied });

    expect(projected.unsaved.totalCount).toBe(0);
    expect(projected.pending.groups[0]?.settings[0]).toMatchObject({
      owner: { setting: "replicas" },
      baselineValue: "1",
      targetValue: "2",
    });
  });

  it("queued coalescing never changes the explicit comparison roles", () => {
    const working = config({ replicas: 3 });
    const saved = config({ replicas: 2 });
    const applied = config({ replicas: 1 });
    const evidence = (replicas: number): CanvasDeploymentEvidence => ({
      id: "queued-attempt",
      status: "queued",
      token: `queued-${replicas}`,
      nodes: [{ node, config: config({ replicas }) }],
    });
    const first = canvasState({
      working,
      saved,
      applied,
      runtime: applied,
      deploymentEvidence: evidence(4),
    });
    const coalesced = canvasState({
      working,
      saved,
      applied,
      runtime: applied,
      deploymentEvidence: evidence(5),
    });

    expect(coalesced.slices).toEqual(first.slices);
    expect(coalesced.deploymentEvidence?.token).toBe("queued-5");
  });

  it("saved deletion retry remains pending until Applied confirms absence", () => {
    const applied = config({ replicas: 1 });
    const pending = changeSet({ working: null, saved: null, applied });
    const completed = changeSet({ working: null, saved: null, applied: null });

    expect(pending.pending.groups[0]?.lifecycle.kind).toBe("delete");
    expect(pending.pending.groups[0]?.discardPlan).toMatchObject({
      kind: "restore",
      target: "saved",
    });
    expect(completed.pending.totalCount).toBe(0);
  });

  it("Discard plans withdraw Saved work without changing Working", () => {
    const working = config({ replicas: 3 });
    const saved = config({ replicas: 2 });
    const applied = config({ replicas: 1 });
    const projected = changeSet({ working, saved, applied });

    expect(projected.pending.discardPlans.settings[0]).toMatchObject({
      kind: "restore_setting",
      target: "saved",
      owner: { setting: "replicas" },
      config: applied,
    });
    expect(projected.unsaved.groups[0]?.settings[0]?.targetValue).toBe("3");
  });

  it("reconnect reveals runtime drift without changing Saved or Applied", () => {
    const desired = config({ replicas: 2 });
    const disconnected = buildCanvasEnvironmentChangeState({
      working: state("working", desired),
      saved: savedState("saved", desired),
      applied: state("applied", desired),
      nodeIntroductions: { token: "introduction", nodes: [] },
      runtimeObserved: null,
      deploymentEvidence: null,
      nodes: [{ node, name: "api", summaryLabel: "api" }],
    });
    const reconnected = canvasState({
      working: desired,
      saved: desired,
      applied: desired,
      runtime: config({ replicas: 1 }),
    });

    expect(disconnected.canDeploy).toBe(false);
    expect(disconnected.slices.drift.totalCount).toBe(0);
    expect(reconnected.canDeploy).toBe(true);
    expect(reconnected.slices.drift.groups[0]?.rows[0]).toMatchObject({
      path: "replicas",
      canDiscard: false,
    });
  });
});
