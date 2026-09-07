import { describe, expect, it } from "vitest";
import { Effect, Schema } from "effect";
import {
  parsePhaseAwareDeployResult,
  type PhaseAwareDeployRequest,
  type ValidatedPhaseAwareDeployResult,
} from "#/modules/runtime/phase-aware-deploy-contract";
import {
  completedPhaseAwareDeployRequestFixture,
  completedPhaseAwareDeployResultFixture,
  interruptedPhaseAwareDeployRequestFixture,
  opportunisticFailurePhaseAwareDeployRequestFixture,
  opportunisticFailurePhaseAwareDeployResultFixture,
  partialPhaseAwareDeployResultFixture,
  requiredFailurePhaseAwareDeployRequestFixture,
} from "#/modules/runtime/phase-aware-deploy-contract.test-fixture";
import { foldPhaseAwareAppliedState } from "#/modules/runtime/phase-aware-applied-state";

type Node = { config: string };

const binding = (serviceId: string, config: string) => ({
  serviceId,
  node: { config },
});

function validatedResult(
  request: PhaseAwareDeployRequest<{
    services: readonly { service_id: string }[];
  }>,
  value: Schema.Json,
) {
  return Effect.runSync(parsePhaseAwareDeployResult(request, value));
}

function fold(input: {
  prior: ReturnType<typeof binding>[];
  target: ReturnType<typeof binding>[];
  result: ValidatedPhaseAwareDeployResult;
}) {
  return Object.fromEntries(
    foldPhaseAwareAppliedState<Node>(input).map(({ serviceId, node }) => [
      serviceId,
      node.config,
    ]),
  );
}

describe("phase-aware Applied State", () => {
  it("advances applied and unchanged Services while satisfying a removal", () => {
    const result = validatedResult(
      completedPhaseAwareDeployRequestFixture,
      completedPhaseAwareDeployResultFixture,
    );

    expect(
      fold({
        prior: [
          binding("old-worker", "old worker applied"),
          binding("cache", "old cache applied"),
        ],
        target: [binding("cache", "saved cache")],
        result,
      }),
    ).toEqual({ cache: "saved cache" });
  });

  it("retains a failed opportunistic Service and promotes later success", () => {
    const result = validatedResult(
      opportunisticFailurePhaseAwareDeployRequestFixture,
      opportunisticFailurePhaseAwareDeployResultFixture,
    );

    expect(
      fold({
        prior: [
          binding("worker", "old worker applied"),
          binding("web", "old web applied"),
        ],
        target: [
          binding("worker", "saved worker"),
          binding("web", "saved web"),
        ],
        result,
      }),
    ).toEqual({
      worker: "old worker applied",
      web: "saved web",
    });
  });

  it("preserves same-phase success and leaves failed and cancelled phases pending", () => {
    const result = validatedResult(
      requiredFailurePhaseAwareDeployRequestFixture,
      partialPhaseAwareDeployResultFixture,
    );

    expect(
      fold({
        prior: [
          binding("database", "old database applied"),
          binding("worker", "old worker applied"),
        ],
        target: [
          binding("database", "saved database"),
          binding("worker", "saved worker"),
          binding("web", "saved web"),
        ],
        result,
      }),
    ).toEqual({
      database: "saved database",
      worker: "old worker applied",
    });
  });

  it("retains prior absence for failed and skipped Services", () => {
    const result = validatedResult(
      requiredFailurePhaseAwareDeployRequestFixture,
      partialPhaseAwareDeployResultFixture,
    );

    expect(
      fold({
        prior: [binding("database", "old database applied")],
        target: [
          binding("database", "saved database"),
          binding("worker", "saved worker"),
          binding("web", "saved web"),
        ],
        result,
      }),
    ).toEqual({ database: "saved database" });
  });

  it("keeps only unambiguous success when execution is interrupted", () => {
    const request = {
      ...interruptedPhaseAwareDeployRequestFixture,
      target: {
        ...interruptedPhaseAwareDeployRequestFixture.target,
        services: [
          { service_id: "database" },
          ...interruptedPhaseAwareDeployRequestFixture.target.services,
        ],
      },
      phases: [
        {
          services: [
            { service_id: "database", requirement: "required" as const },
            ...interruptedPhaseAwareDeployRequestFixture.phases[0].services,
          ],
        },
        interruptedPhaseAwareDeployRequestFixture.phases[1],
      ],
    };
    const result = validatedResult(request, {
      version: 1,
      outcome: "interrupted",
      phases: [
        {
          phase: 0,
          outcome: "interrupted",
          services: [
            { service_id: "database", result: "applied" },
            {
              service_id: "api",
              result: "interrupted",
              interruption: {
                code: "commit_ambiguous",
                message: "Runtime could not prove whether the commit landed.",
              },
            },
          ],
        },
        {
          phase: 1,
          outcome: "skipped",
          services: [
            {
              service_id: "web",
              result: "skipped",
              reason: {
                code: "deploy_interrupted",
                message: "An earlier phase was interrupted.",
              },
            },
          ],
        },
      ],
    });

    expect(
      fold({
        prior: [
          binding("database", "old database applied"),
          binding("api", "old api applied"),
        ],
        target: [
          binding("database", "saved database"),
          binding("api", "saved api"),
          binding("web", "saved web"),
        ],
        result,
      }),
    ).toEqual({
      database: "saved database",
      api: "old api applied",
    });
  });
});
