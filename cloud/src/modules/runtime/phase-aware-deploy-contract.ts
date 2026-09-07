import { Data, Effect, Schema } from "effect";
import { finiteNumber, strictParseOptions } from "#/modules/environment-design/schema";

export type DeploymentRequirement = "required" | "opportunistic";

export type PhaseAwareDeployAction = {
  readonly service_id: string;
  readonly requirement: DeploymentRequirement;
};

export type PhaseAwareDeployPhase = {
  readonly services: readonly PhaseAwareDeployAction[];
};

/**
 * Cloud's desired runtime request. `target` is the complete desired Namespace;
 * `phases` contains only affected Service actions in dependency order. A phase
 * reference present in `target.services` is an apply, while an absent reference
 * is a removal. A Service may appear in exactly one phase.
 */
export type PhaseAwareDeployRequest<
  TTarget extends { readonly services: readonly { readonly service_id: string }[] },
> = {
  readonly version: 1;
  readonly target: TTarget;
  readonly phases: readonly PhaseAwareDeployPhase[];
};

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const ResultEvidence = Schema.Struct({
  code: NonEmptyString,
  message: NonEmptyString,
});
const PhaseAwareDeployServiceResultSchema = Schema.Union([
  Schema.Struct({
    service_id: NonEmptyString,
    result: Schema.Literals(["applied", "removed", "unchanged"]),
  }),
  Schema.Struct({
    service_id: NonEmptyString,
    result: Schema.Literal("failed"),
    failure: ResultEvidence,
  }),
  Schema.Struct({
    service_id: NonEmptyString,
    result: Schema.Literal("skipped"),
    reason: ResultEvidence,
  }),
  Schema.Struct({
    service_id: NonEmptyString,
    result: Schema.Literal("interrupted"),
    interruption: ResultEvidence,
  }),
]);
const PhaseAwareDeployResultSchema = Schema.Struct({
  version: Schema.Literal(1),
  outcome: Schema.Literals(["completed", "partial", "failed", "interrupted"]),
  phases: Schema.Array(
    Schema.Struct({
      phase: finiteNumber({ integer: true, minimum: 0 }),
      outcome: Schema.Literals([
        "completed",
        "partial",
        "failed",
        "skipped",
        "interrupted",
      ]),
      services: Schema.Array(PhaseAwareDeployServiceResultSchema),
    }),
  ),
});

export type PhaseAwareDeployResult = typeof PhaseAwareDeployResultSchema.Type;
declare const validatedPhaseAwareDeployResult: unique symbol;
export type ValidatedPhaseAwareDeployResult = PhaseAwareDeployResult & {
  readonly [validatedPhaseAwareDeployResult]: true;
};
export type PhaseAwareDeployServiceResult =
  PhaseAwareDeployResult["phases"][number]["services"][number];

export class PhaseAwareDeployValidationError extends Data.TaggedError(
  "PhaseAwareDeployValidationError",
)<{
  readonly field: "phases" | "result" | "result.phases";
  readonly message: string;
  readonly cause?: unknown;
}> {}

export function deploymentRequirementFor(input: {
  readonly trigger: "manual" | "git";
  readonly sourceAffected: boolean;
  readonly alreadyApplied: boolean;
}): DeploymentRequirement {
  if (input.trigger === "manual") return "required";
  return input.sourceAffected && input.alreadyApplied
    ? "required"
    : "opportunistic";
}

/** Freeze planner evidence and removal policy into the immutable request. */
export function createPhaseAwareDeployRequestFromPlan<
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
}): Effect.Effect<
  PhaseAwareDeployRequest<TTarget>,
  PhaseAwareDeployValidationError
> {
  const sourceAffected = new Set(input.sourceAffectedServiceIds);
  const alreadyApplied = new Set(input.alreadyAppliedServiceIds);
  const actionFor = (serviceId: string): PhaseAwareDeployAction => ({
    service_id: serviceId,
    requirement: deploymentRequirementFor({
      trigger: input.trigger,
      sourceAffected: sourceAffected.has(serviceId),
      alreadyApplied: alreadyApplied.has(serviceId),
    }),
  });
  const phases = input.plannedPhases.map((phase) => ({
    services: phase.services.map((service) => actionFor(service.service_id)),
  }));
  if (input.removalServiceIds.length > 0) {
    const removals = input.removalServiceIds.map(actionFor);
    const finalPhase = phases.at(-1);
    if (finalPhase) finalPhase.services.push(...removals);
    else phases.push({ services: removals });
  }
  return createPhaseAwareDeployRequest({ target: input.target, phases });
}

/** Enforce cross-phase Service identity while preserving evidence order. */
export function createPhaseAwareDeployRequest<
  TTarget extends { readonly services: readonly { readonly service_id: string }[] },
>(input: {
  readonly target: TTarget;
  readonly phases: readonly PhaseAwareDeployPhase[];
}): Effect.Effect<
  PhaseAwareDeployRequest<TTarget>,
  PhaseAwareDeployValidationError
> {
  const seen = new Set<string>();
  for (const phase of input.phases) {
    for (const action of phase.services) {
      if (seen.has(action.service_id)) {
        return Effect.fail(
          new PhaseAwareDeployValidationError({
            field: "phases",
            message: `Service ${action.service_id} appears in more than one deploy phase.`,
          }),
        );
      }
      seen.add(action.service_id);
    }
  }
  return Effect.succeed({
    version: 1,
    target: input.target,
    phases: input.phases,
  });
}

function resultError(message: string) {
  return new PhaseAwareDeployValidationError({
    field: "result.phases",
    message,
  });
}

function validateResult(
  request: PhaseAwareDeployRequest<{
    readonly services: readonly { readonly service_id: string }[];
  }>,
  parsed: PhaseAwareDeployResult,
): PhaseAwareDeployValidationError | null {
  if (parsed.phases.length !== request.phases.length) {
    return resultError(
      `Result has ${parsed.phases.length} phases; expected ${request.phases.length}.`,
    );
  }
  const targetServiceIds = new Set(
    request.target.services.map((service) => service.service_id),
  );
  const seen = new Set<string>();
  let laterPhasesMustSkip = false;
  let hasRequiredFailure = false;
  let hasOpportunisticFailure = false;
  let hasInterruption = false;
  for (const [expectedPhase, phase] of parsed.phases.entries()) {
    const requestedPhase = request.phases[expectedPhase];
    if (!requestedPhase) {
      return resultError(`Result phase ${phase.phase} is unexpected.`);
    }
    if (phase.phase !== expectedPhase) {
      return resultError(
        `Result phase ${phase.phase} is out of order; expected phase ${expectedPhase}.`,
      );
    }
    if (phase.services.length !== requestedPhase.services.length) {
      return resultError(
        `Result phase ${expectedPhase} has ${phase.services.length} Services; expected ${requestedPhase.services.length}.`,
      );
    }
    let requiredFailure = false;
    let interrupted = false;
    let anyFailure = false;
    let anySkipped = false;
    for (const [serviceIndex, service] of phase.services.entries()) {
      const requested = requestedPhase.services[serviceIndex];
      if (seen.has(service.service_id)) {
        return resultError(
          `Service ${service.service_id} appears in more than one result phase.`,
        );
      }
      seen.add(service.service_id);
      if (!requested || service.service_id !== requested.service_id) {
        return resultError(
          `Result phase ${expectedPhase} Service ${service.service_id} is out of order.`,
        );
      }
      if (
        service.result === "applied" &&
        !targetServiceIds.has(service.service_id)
      ) {
        return resultError(
          `Removed Service ${service.service_id} cannot have an applied result.`,
        );
      }
      if (
        service.result === "removed" &&
        targetServiceIds.has(service.service_id)
      ) {
        return resultError(
          `Present Service ${service.service_id} cannot have a removed result.`,
        );
      }
      if (service.result === "failed") {
        anyFailure = true;
        requiredFailure ||= requested.requirement === "required";
      }
      interrupted ||= service.result === "interrupted";
      anySkipped ||= service.result === "skipped";
    }
    if (
      laterPhasesMustSkip &&
      (phase.outcome !== "skipped" ||
        phase.services.some((service) => service.result !== "skipped"))
    ) {
      return resultError(
        `Result phase ${expectedPhase} must be skipped after an earlier required failure or interruption.`,
      );
    }
    if (!laterPhasesMustSkip) {
      const expectedOutcome = interrupted
        ? "interrupted"
        : requiredFailure
          ? "failed"
          : anyFailure
            ? "partial"
            : "completed";
      if (anySkipped) {
        return resultError(
          `Result phase ${expectedPhase} cannot skip a Service before a required failure or interruption.`,
        );
      }
      if (phase.outcome !== expectedOutcome) {
        return resultError(
          `Result phase ${expectedPhase} must be ${expectedOutcome} for its Service evidence.`,
        );
      }
      hasRequiredFailure ||= requiredFailure;
      hasOpportunisticFailure ||= anyFailure && !requiredFailure;
      hasInterruption ||= interrupted;
      laterPhasesMustSkip = requiredFailure || interrupted;
    }
  }
  const expectedOutcome = hasInterruption
    ? "interrupted"
    : hasRequiredFailure
      ? "failed"
      : hasOpportunisticFailure
        ? "partial"
        : "completed";
  return parsed.outcome === expectedOutcome
    ? null
    : resultError(
        `Deploy outcome must be ${expectedOutcome} for its Service evidence.`,
      );
}

export function parsePhaseAwareDeployResult(
  request: PhaseAwareDeployRequest<{
    readonly services: readonly { readonly service_id: string }[];
  }>,
  value: Schema.Json,
): Effect.Effect<
  ValidatedPhaseAwareDeployResult,
  PhaseAwareDeployValidationError
> {
  return Schema.decodeUnknownEffect(PhaseAwareDeployResultSchema)(
    value,
    strictParseOptions,
  ).pipe(
    Effect.mapError(
      (cause) =>
        new PhaseAwareDeployValidationError({
          field: "result",
          message: "Phase-aware deploy result is invalid.",
          cause,
        }),
    ),
    Effect.flatMap((parsed) => {
      const error = validateResult(request, parsed);
      return error
        ? Effect.fail(error)
        : Effect.succeed(
            // SAFETY: strict schema decoding and all request-relative invariants passed.
            parsed as ValidatedPhaseAwareDeployResult,
          );
    }),
  );
}

export function parsePhaseAwareDeployResultFromPhaseEvidence<
  TTarget extends { readonly services: readonly { readonly service_id: string }[] },
>(
  request: PhaseAwareDeployRequest<TTarget>,
  evidence: readonly {
    readonly phase: number;
    readonly services: readonly PhaseAwareDeployServiceResult[];
  }[],
): Effect.Effect<
  ValidatedPhaseAwareDeployResult,
  PhaseAwareDeployValidationError
> {
  const phases: PhaseAwareDeployResult["phases"] = evidence.map(
    (phase, phaseIndex) => {
      const interrupted = phase.services.some(
        (service) => service.result === "interrupted",
      );
      const requiredFailure = phase.services.some(
        (service, serviceIndex) =>
          service.result === "failed" &&
          request.phases[phaseIndex]?.services[serviceIndex]?.requirement ===
            "required",
      );
      const anyFailure = phase.services.some(
        (service) => service.result === "failed",
      );
      const allSkipped =
        phase.services.length > 0 &&
        phase.services.every((service) => service.result === "skipped");
      return {
        phase: phase.phase,
        outcome: allSkipped
          ? "skipped"
          : interrupted
            ? "interrupted"
            : requiredFailure
              ? "failed"
              : anyFailure
                ? "partial"
                : "completed",
        services: phase.services,
      };
    },
  );
  const hasInterruption = phases.some((phase) =>
    phase.services.some((service) => service.result === "interrupted"),
  );
  const hasRequiredFailure = phases.some((phase, phaseIndex) =>
    phase.services.some(
      (service, serviceIndex) =>
        service.result === "failed" &&
        request.phases[phaseIndex]?.services[serviceIndex]?.requirement ===
          "required",
    ),
  );
  const hasFailure = phases.some((phase) =>
    phase.services.some((service) => service.result === "failed"),
  );
  return parsePhaseAwareDeployResult(request, {
    version: 1,
    phases,
    outcome: hasInterruption
      ? "interrupted"
      : hasRequiredFailure
        ? "failed"
        : hasFailure
          ? "partial"
          : "completed",
  });
}
