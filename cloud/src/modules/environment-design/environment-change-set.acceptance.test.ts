import { describe, expect, it } from "vitest";
import { Effect, Schema } from "effect";
import { Result } from "effect";
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
import { foldPhaseAwareAppliedState } from "#/modules/runtime/phase-aware-applied-state";
import {
  createPhaseAwareDeployRequestFromPlan as createPhaseAwareDeployRequestFromPlanEffect,
  parsePhaseAwareDeployResult as parsePhaseAwareDeployResultEffect,
} from "#/modules/runtime/phase-aware-deploy-contract";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";

function syncResult<A, E>(program: Effect.Effect<A, E>) {
  return Effect.runSync(
    Effect.match(program, {
      onFailure: (error) => Result.fail(error),
      onSuccess: (value) => Result.succeed(value),
    }),
  );
}

function createPhaseAwareDeployRequestFromPlan<
  TTarget extends { readonly services: readonly { readonly service_id: string }[] },
>(input: {
  readonly target: TTarget;
  readonly plannedPhases: readonly {
    readonly services: readonly { readonly service_id: string }[];
  }[];
  readonly removalServiceIds: readonly string[];
  readonly trigger: "manual" | "git";
  readonly sourceAffectedServiceIds: readonly string[];
  readonly alreadyAppliedServiceIds: readonly string[];
}) {
  return syncResult(createPhaseAwareDeployRequestFromPlanEffect(input));
}

function parsePhaseAwareDeployResult(
  request: Parameters<typeof parsePhaseAwareDeployResultEffect>[0],
  value: Schema.Json,
) {
  return syncResult(parsePhaseAwareDeployResultEffect(request, value));
}

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
  it("save then Git publishes the saved configuration with the source overlay", () => {
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

    const request = createPhaseAwareDeployRequestFromPlan({
      target: {
        services: [
          {
            service_id: "api",
            image: "registry.test/api:git-sha",
            env: saved.env,
          },
        ],
      },
      plannedPhases: [{ services: [{ service_id: "api" }] }],
      removalServiceIds: [],
      trigger: "git",
      sourceAffectedServiceIds: ["api"],
      alreadyAppliedServiceIds: ["api"],
    });
    expect(Result.isSuccess(request)).toBe(true);
    if (Result.isFailure(request)) throw request.failure;
    expect(request.success.target.services[0]).toMatchObject({
      image: "registry.test/api:git-sha",
      env: { FEATURE: { kind: "literal", value: "saved" } },
    });
    expect(request.success.target.services[0]?.env).not.toEqual(
      successorWorking.env,
    );
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

  it("partial success advances only the confirmed Service", () => {
    const request = {
      version: 1 as const,
      target: {
        services: [
          { service_id: "database" },
          { service_id: "api" },
        ],
      },
      phases: [
        {
          services: [
            { service_id: "database", requirement: "required" as const },
            { service_id: "api", requirement: "required" as const },
          ],
        },
      ],
    };
    const result = parsePhaseAwareDeployResult(request, {
      version: 1,
      outcome: "failed",
      phases: [
        {
          phase: 0,
          outcome: "failed",
          services: [
            { service_id: "database", result: "applied" },
            {
              service_id: "api",
              result: "failed",
              failure: { code: "healthcheck", message: "unhealthy" },
            },
          ],
        },
      ],
    });
    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) throw result.failure;
    const folded = foldPhaseAwareAppliedState({
      prior: [
        { serviceId: "database", node: "database-old" },
        { serviceId: "api", node: "api-old" },
      ],
      target: [
        { serviceId: "database", node: "database-saved" },
        { serviceId: "api", node: "api-saved" },
      ],
      result: result.success,
    });

    expect(
      Object.fromEntries(folded.map((item) => [item.serviceId, item.node])),
    ).toEqual({ database: "database-saved", api: "api-old" });
  });

  it("opportunistic introduction remains absent on failure and applies later", () => {
    const target = [{ serviceId: "worker", node: "worker-saved" }];
    const request = createPhaseAwareDeployRequestFromPlan({
      target: { services: [{ service_id: "worker" }] },
      plannedPhases: [{ services: [{ service_id: "worker" }] }],
      removalServiceIds: [],
      trigger: "git",
      sourceAffectedServiceIds: ["worker"],
      alreadyAppliedServiceIds: [],
    });
    expect(Result.isSuccess(request)).toBe(true);
    if (Result.isFailure(request)) throw request.failure;
    expect(request.success.phases[0]?.services[0]?.requirement).toBe(
      "opportunistic",
    );
    const failedResult = parsePhaseAwareDeployResult(request.success, {
      version: 1,
      outcome: "partial",
      phases: [
        {
          phase: 0,
          outcome: "partial",
          services: [
            {
              service_id: "worker",
              result: "failed",
              failure: { code: "healthcheck", message: "unhealthy" },
            },
          ],
        },
      ],
    });
    expect(Result.isSuccess(failedResult)).toBe(true);
    if (Result.isFailure(failedResult)) throw failedResult.failure;
    const failed = foldPhaseAwareAppliedState({
      prior: [],
      target,
      result: failedResult.success,
    });
    const recoveredResult = parsePhaseAwareDeployResult(request.success, {
      version: 1,
      outcome: "completed",
      phases: [
        {
          phase: 0,
          outcome: "completed",
          services: [{ service_id: "worker", result: "applied" }],
        },
      ],
    });
    expect(Result.isSuccess(recoveredResult)).toBe(true);
    if (Result.isFailure(recoveredResult)) throw recoveredResult.failure;
    const recovered = foldPhaseAwareAppliedState({
      prior: failed,
      target,
      result: recoveredResult.success,
    });

    expect(failed).toEqual([]);
    expect(recovered).toEqual(target);
  });

  it("required phase failure rejects continued later-phase execution", () => {
    const request = {
      version: 1 as const,
      target: { services: [{ service_id: "database" }, { service_id: "api" }] },
      phases: [
        {
          services: [
            { service_id: "database", requirement: "required" as const },
          ],
        },
        { services: [{ service_id: "api", requirement: "required" as const }] },
      ],
    };
    const result = parsePhaseAwareDeployResult(request, {
      version: 1,
      outcome: "failed",
      phases: [
        {
          phase: 0,
          outcome: "failed",
          services: [
            {
              service_id: "database",
              result: "failed",
              failure: { code: "healthcheck", message: "unhealthy" },
            },
          ],
        },
        {
          phase: 1,
          outcome: "completed",
          services: [{ service_id: "api", result: "applied" }],
        },
      ],
    });

    expect(Result.isFailure(result)).toBe(true);
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
