import { describe, expect, it } from "vitest";
import { Effect, Schema } from "effect";
import { Result } from "effect";
import {
  createPhaseAwareDeployRequest as createPhaseAwareDeployRequestEffect,
  createPhaseAwareDeployRequestFromPlan as createPhaseAwareDeployRequestFromPlanEffect,
  deploymentRequirementFor,
  parsePhaseAwareDeployResult as parsePhaseAwareDeployResultEffect,
  parsePhaseAwareDeployResultFromPhaseEvidence as parsePhaseAwareDeployResultFromPhaseEvidenceEffect,
  type PhaseAwareDeployPhase,
} from "#/modules/runtime/phase-aware-deploy-contract";
import {
  completedPhaseAwareDeployRequestFixture,
  completedPhaseAwareDeployResultFixture,
  interruptedPhaseAwareDeployResultFixture,
  interruptedPhaseAwareDeployRequestFixture,
  opportunisticFailurePhaseAwareDeployRequestFixture,
  opportunisticFailurePhaseAwareDeployResultFixture,
  phaseAwareDeployRequestFixture,
  partialPhaseAwareDeployResultFixture,
  requiredFailurePhaseAwareDeployRequestFixture,
} from "#/modules/runtime/phase-aware-deploy-contract.test-fixture";

function syncResult<A, E>(program: Effect.Effect<A, E>) {
  return Effect.runSync(
    Effect.match(program, {
      onFailure: (error) => Result.fail(error),
      onSuccess: (value) => Result.succeed(value),
    }),
  );
}

function createPhaseAwareDeployRequest<
  TTarget extends { readonly services: readonly { readonly service_id: string }[] },
>(input: { readonly target: TTarget; readonly phases: readonly PhaseAwareDeployPhase[] }) {
  return syncResult(createPhaseAwareDeployRequestEffect(input));
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

function parsePhaseAwareDeployResultFromPhaseEvidence<
  TTarget extends { readonly services: readonly { readonly service_id: string }[] },
>(
  request: import("#/modules/runtime/phase-aware-deploy-contract").PhaseAwareDeployRequest<TTarget>,
  evidence: Parameters<typeof parsePhaseAwareDeployResultFromPhaseEvidenceEffect>[1],
) {
  return syncResult(
    parsePhaseAwareDeployResultFromPhaseEvidenceEffect(request, evidence),
  );
}

describe("phase-aware deploy request contract", () => {
  it("serializes one complete target with ordered required and opportunistic actions", () => {
    const { target } = phaseAwareDeployRequestFixture;

    const result = createPhaseAwareDeployRequest({
      target,
      phases: [
        {
          services: [
            {
              service_id: "database",
              requirement: deploymentRequirementFor({
                trigger: "git",
                sourceAffected: true,
                alreadyApplied: true,
              }),
            },
            {
              service_id: "retired-worker",
              requirement: deploymentRequirementFor({
                trigger: "git",
                sourceAffected: false,
                alreadyApplied: true,
              }),
            },
          ],
        },
        {
          services: [
            {
              service_id: "worker",
              requirement: deploymentRequirementFor({
                trigger: "git",
                sourceAffected: false,
                alreadyApplied: false,
              }),
            },
          ],
        },
      ],
    });

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) throw result.failure;
    expect(result.success).toEqual(phaseAwareDeployRequestFixture);
    expect(JSON.parse(JSON.stringify(result.success))).toEqual(result.success);
    expect(
      deploymentRequirementFor({
        trigger: "manual",
        sourceAffected: false,
        alreadyApplied: false,
      }),
    ).toBe("required");
  });

  it("rejects a Service action repeated across dependency phases", () => {
    const result = createPhaseAwareDeployRequest({
      target: {
        namespace_id: "production",
        services: [{ service_id: "api" }],
      },
      phases: [
        { services: [{ service_id: "api", requirement: "required" }] },
        { services: [{ service_id: "api", requirement: "opportunistic" }] },
      ],
    });

    expect(Result.isFailure(result)).toBe(true);
    if (Result.isSuccess(result)) throw new Error("Expected duplicate rejection");
    expect(result.failure).toMatchObject({
      _tag: "PhaseAwareDeployValidationError",
      field: "phases",
      message: "Service api appears in more than one deploy phase.",
    });
  });

  it("freezes planner order, removals, and request policy", () => {
    const gitRequest = createPhaseAwareDeployRequestFromPlan({
      target: phaseAwareDeployRequestFixture.target,
      plannedPhases: [
        { services: [{ service_id: "database" }] },
        { services: [{ service_id: "worker" }] },
      ],
      removalServiceIds: ["retired-worker"],
      trigger: "git",
      sourceAffectedServiceIds: ["database", "worker"],
      alreadyAppliedServiceIds: ["database", "retired-worker"],
    });
    const manualRequest = createPhaseAwareDeployRequestFromPlan({
      target: phaseAwareDeployRequestFixture.target,
      plannedPhases: [{ services: [{ service_id: "worker" }] }],
      removalServiceIds: ["retired-worker"],
      trigger: "manual",
      sourceAffectedServiceIds: [],
      alreadyAppliedServiceIds: [],
    });

    expect(Result.isSuccess(gitRequest)).toBe(true);
    if (Result.isFailure(gitRequest)) throw gitRequest.failure;
    expect(gitRequest.success.phases).toEqual([
      {
        services: [{ service_id: "database", requirement: "required" }],
      },
      {
        services: [
          { service_id: "worker", requirement: "opportunistic" },
          {
            service_id: "retired-worker",
            requirement: "opportunistic",
          },
        ],
      },
    ]);
    expect(Result.isSuccess(manualRequest)).toBe(true);
    if (Result.isFailure(manualRequest)) throw manualRequest.failure;
    expect(manualRequest.success.phases[0]?.services).toEqual([
      { service_id: "worker", requirement: "required" },
      { service_id: "retired-worker", requirement: "required" },
    ]);
  });
});

describe("phase-aware deploy result contract", () => {
  it.each([
    [
      completedPhaseAwareDeployRequestFixture,
      completedPhaseAwareDeployResultFixture,
    ],
    [
      requiredFailurePhaseAwareDeployRequestFixture,
      partialPhaseAwareDeployResultFixture,
    ],
    [
      opportunisticFailurePhaseAwareDeployRequestFixture,
      opportunisticFailurePhaseAwareDeployResultFixture,
    ],
    [
      interruptedPhaseAwareDeployRequestFixture,
      interruptedPhaseAwareDeployResultFixture,
    ],
  ] as const)("validates ordered request-aware Service evidence", (request, fixture) => {
    // SAFETY: JSON round-tripping a serializable fixture produces a JSON value.
    const wireValue = JSON.parse(JSON.stringify(fixture)) as Schema.Json;

    const result = parsePhaseAwareDeployResult(request, wireValue);

    expect(Result.isSuccess(result)).toBe(true);
    if (Result.isFailure(result)) throw result.failure;
    expect(result.success).toEqual(fixture);
  });

  it("rejects duplicate Service evidence and noncontiguous phase order", () => {
    const duplicate = parsePhaseAwareDeployResult(
      requiredFailurePhaseAwareDeployRequestFixture,
      {
        ...partialPhaseAwareDeployResultFixture,
        phases: [
          partialPhaseAwareDeployResultFixture.phases[0],
          {
            phase: 1,
            outcome: "completed",
            services: [{ service_id: "database", result: "unchanged" }],
          },
        ],
      },
    );
    const outOfOrder = parsePhaseAwareDeployResult(
      completedPhaseAwareDeployRequestFixture,
      {
        ...completedPhaseAwareDeployResultFixture,
        phases: [
          {
            ...completedPhaseAwareDeployResultFixture.phases[0],
            phase: 1,
          },
        ],
      },
    );

    expect(Result.isFailure(duplicate)).toBe(true);
    if (Result.isSuccess(duplicate)) throw new Error("Expected duplicate rejection");
    expect(duplicate.failure.message).toBe(
      "Service database appears in more than one result phase.",
    );
    expect(Result.isFailure(outOfOrder)).toBe(true);
    if (Result.isSuccess(outOfOrder)) throw new Error("Expected order rejection");
    expect(outOfOrder.failure.message).toBe(
      "Result phase 1 is out of order; expected phase 0.",
    );
  });

  it("rejects undeclared result fields and failure without typed evidence", () => {
    const extraField = parsePhaseAwareDeployResult(
      completedPhaseAwareDeployRequestFixture,
      {
        ...completedPhaseAwareDeployResultFixture,
        internal_debug: "secret",
      },
    );
    const missingFailure = parsePhaseAwareDeployResult(
      {
        version: 1,
        target: { services: [{ service_id: "api" }] },
        phases: [
          { services: [{ service_id: "api", requirement: "required" }] },
        ],
      },
      {
        version: 1,
        outcome: "failed",
        phases: [
          {
            phase: 0,
            outcome: "failed",
            services: [{ service_id: "api", result: "failed" }],
          },
        ],
      },
    );

    expect(Result.isFailure(extraField)).toBe(true);
    expect(Result.isFailure(missingFailure)).toBe(true);
  });

  it("rejects continued execution after required failure or interruption", () => {
    const afterRequiredFailure = parsePhaseAwareDeployResult(
      requiredFailurePhaseAwareDeployRequestFixture,
      {
        ...partialPhaseAwareDeployResultFixture,
        phases: [
          partialPhaseAwareDeployResultFixture.phases[0],
          {
            phase: 1,
            outcome: "completed",
            services: [{ service_id: "web", result: "applied" }],
          },
        ],
      },
    );
    const afterInterruption = parsePhaseAwareDeployResult(
      interruptedPhaseAwareDeployRequestFixture,
      {
        ...interruptedPhaseAwareDeployResultFixture,
        phases: [
          interruptedPhaseAwareDeployResultFixture.phases[0],
          {
            phase: 1,
            outcome: "completed",
            services: [{ service_id: "web", result: "applied" }],
          },
        ],
      },
    );

    expect(Result.isFailure(afterRequiredFailure)).toBe(true);
    expect(Result.isFailure(afterInterruption)).toBe(true);
  });

  it("refuses to reconcile phase evidence that continued after a required failure", () => {
    const result = parsePhaseAwareDeployResultFromPhaseEvidence(
      requiredFailurePhaseAwareDeployRequestFixture,
      [
        {
          phase: 0,
          services: partialPhaseAwareDeployResultFixture.phases[0]?.services ?? [],
        },
        {
          phase: 1,
          services: [{ service_id: "web", result: "applied" }],
        },
      ],
    );

    expect(Result.isFailure(result)).toBe(true);
  });

  it("rejects outcomes that contradict required and opportunistic policy", () => {
    const requiredReportedPartial = parsePhaseAwareDeployResult(
      requiredFailurePhaseAwareDeployRequestFixture,
      { ...partialPhaseAwareDeployResultFixture, outcome: "partial" },
    );
    const opportunisticReportedFailed = parsePhaseAwareDeployResult(
      opportunisticFailurePhaseAwareDeployRequestFixture,
      {
        ...opportunisticFailurePhaseAwareDeployResultFixture,
        outcome: "failed",
      },
    );

    expect(Result.isFailure(requiredReportedPartial)).toBe(true);
    expect(Result.isFailure(opportunisticReportedFailed)).toBe(true);
  });
});
