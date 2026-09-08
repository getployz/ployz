import type {
  ClusterTeardown,
  DeployOutcome,
  ExecutionError,
} from "@ployz/sdk";
import type { DataLossList } from "#/modules/runtime/data-loss-confirm";
import { dataLossIdentitySchema } from "#/modules/runtime/data-loss-identity";
import { Schema } from "effect";

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const Uuid = Schema.String.check(Schema.isUUID());

export const TEARDOWN_SCOPES = [
  "environment",
  "project",
  "organization",
] as const;

export type TeardownScope = (typeof TEARDOWN_SCOPES)[number];

export const TeardownTargetInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  scope: Schema.Literals(TEARDOWN_SCOPES),
  environmentId: Schema.optionalKey(Uuid),
  projectSlug: Schema.optionalKey(NonEmptyString),
});
export type TeardownTargetInput = typeof TeardownTargetInput.Type;

export const ConfirmTeardownInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  scope: Schema.Literals(TEARDOWN_SCOPES),
  environmentId: Schema.optionalKey(Uuid),
  projectSlug: Schema.optionalKey(NonEmptyString),
  identities: Schema.Array(dataLossIdentitySchema),
  abandon: Schema.optionalKey(Schema.Boolean),
});
export type ConfirmTeardownInput = typeof ConfirmTeardownInput.Type;

export const RetryTeardownInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  attemptId: Uuid,
});
export type RetryTeardownInput = typeof RetryTeardownInput.Type;

export const TEARDOWN_ATTEMPT_STATUSES = [
  "pending",
  "running",
  "completed",
  "partial",
  "failed",
  "cancelled",
] as const;

export type TeardownAttemptStatus =
  (typeof TEARDOWN_ATTEMPT_STATUSES)[number];

const TeardownEnvironmentTargetSchema = Schema.Struct({
  environmentId: NonEmptyString,
  projectId: NonEmptyString,
  projectName: NonEmptyString,
  cloudName: NonEmptyString,
});
export type TeardownEnvironmentTarget =
  typeof TeardownEnvironmentTargetSchema.Type;

const TeardownRuntimeMembershipSchema = Schema.Literals([
  "verified",
  "unknown",
  "untouched",
]);
export type TeardownRuntimeMembership =
  typeof TeardownRuntimeMembershipSchema.Type;

export type TeardownRuntimeOutcome = "verified_zero" | "unknown" | "untouched";

const TeardownTargetsSchema = Schema.Struct({
  environments: Schema.Array(TeardownEnvironmentTargetSchema),
  destroyRuntimeProjects: Schema.Boolean,
  revokePairing: Schema.Boolean,
  runtimeMembership: TeardownRuntimeMembershipSchema,
});
export type TeardownTargets = typeof TeardownTargetsSchema.Type;

export function parseTeardownTargets<Input>(targets: Input): TeardownTargets {
  return Schema.decodeUnknownSync(TeardownTargetsSchema)(targets, {
    onExcessProperty: "error",
  });
}

export type TeardownRuntimeEvidence = {
  projectTeardowns?: Array<{
    projectName: string;
    outcome: DeployOutcome<ExecutionError>;
  }>;
  clusterTeardown?: ClusterTeardown;
};

export type TeardownOutcome =
  | ({
      rustMustRevokePairing: false;
      runtimeMembership: TeardownRuntimeOutcome;
    } & TeardownRuntimeEvidence)
  | ({
      rustMustRevokePairing: true;
      runtimeMembership: "unknown";
    } & TeardownRuntimeEvidence);

export type TeardownClusterView =
  | { kind: "no_cluster" }
  | { kind: "unreachable" }
  | { kind: "live" };

export type TeardownRuntimePlan =
  | {
      kind: "ok";
      revokePairing: boolean;
      runtimeMembership: TeardownRuntimeMembership;
    }
  | {
      kind: "refuse";
      reason: "use_abandon" | "use_verified" | "nothing_to_abandon";
    };

/**
 * Org teardown pins live Cluster membership when Dial works. An unreachable
 * Cluster cannot be recorded as verified zero — that takes an explicit abandon.
 */
export function planTeardownRuntime(input: {
  scope: TeardownScope;
  abandon: boolean;
  cluster: TeardownClusterView;
}): TeardownRuntimePlan {
  if (input.scope !== "organization") {
    return {
      kind: "ok",
      revokePairing: false,
      runtimeMembership: "untouched",
    };
  }
  if (input.abandon) {
    if (input.cluster.kind !== "unreachable") {
      return {
        kind: "refuse",
        reason:
          input.cluster.kind === "live" ? "use_verified" : "nothing_to_abandon",
      };
    }
    return {
      kind: "ok",
      revokePairing: true,
      runtimeMembership: "unknown",
    };
  }
  switch (input.cluster.kind) {
    case "unreachable":
      return { kind: "refuse", reason: "use_abandon" };
    case "no_cluster":
      return {
        kind: "ok",
        revokePairing: false,
        runtimeMembership: "untouched",
      };
    case "live":
      return {
        kind: "ok",
        revokePairing: true,
        runtimeMembership: "verified",
      };
    default: {
      const exhaustive: never = input.cluster;
      return exhaustive;
    }
  }
}

export function teardownRuntimeRefuseMessage(
  reason: Extract<TeardownRuntimePlan, { kind: "refuse" }>["reason"],
): string {
  switch (reason) {
    case "use_abandon":
      return "The cluster is expected but unreachable. Abandon it to drop Cloud management without a verified runtime teardown.";
    case "use_verified":
      return "The cluster is reachable. Tear it down instead of abandoning.";
    case "nothing_to_abandon":
      return "There is no cluster to abandon.";
    default: {
      const exhaustive: never = reason;
      return exhaustive;
    }
  }
}

export function teardownCompletedDescription(
  outcome: TeardownOutcome,
): string {
  const membership = outcome.runtimeMembership;
  switch (membership) {
    case "unknown":
      return outcome.rustMustRevokePairing
        ? "Cloud management was dropped. Runtime membership remains unknown, and pairing must still be revoked in Rust."
        : "Cloud management was dropped. Runtime membership remains unknown.";
    case "verified_zero":
      return "The cluster was removed. Cloud recorded verified zero.";
    case "untouched":
      return "Confirmed rust work ran, then Cloud rows were dropped.";
    default: {
      const exhaustive: never = membership;
      return exhaustive;
    }
  }
}

function teardownRuntimeOutcome(
  membership: TeardownRuntimeMembership,
): TeardownRuntimeOutcome {
  switch (membership) {
    case "verified":
      return "verified_zero";
    case "unknown":
      return "unknown";
    case "untouched":
      return "untouched";
    default: {
      const exhaustive: never = membership;
      return exhaustive;
    }
  }
}

export function teardownOutcome(
  membership: TeardownRuntimeMembership,
  rustMustRevokePairing: boolean,
  evidence: TeardownRuntimeEvidence = {},
): TeardownOutcome {
  if (rustMustRevokePairing) {
    return {
      rustMustRevokePairing: true,
      runtimeMembership: "unknown",
      ...evidence,
    };
  }
  return {
    rustMustRevokePairing: false,
    runtimeMembership: teardownRuntimeOutcome(membership),
    ...evidence,
  };
}

/** A partial Cluster result is evidence of remaining unknown runtime state. */
export function incompleteTeardownOutcome(
  membership: TeardownRuntimeMembership,
  evidence: TeardownRuntimeEvidence = {},
): TeardownOutcome {
  return teardownOutcome(
    membership === "verified" ? "unknown" : membership,
    false,
    evidence,
  );
}

export function cloudEnvironmentName(input: {
  organizationSlug: string;
  projectSlug: string;
  environmentName: string;
}): string {
  return `${input.organizationSlug}/${input.projectSlug}/${input.environmentName}`;
}

export function environmentCloudRows(input: {
  cloudName: string;
  services: readonly { name: string }[];
  volumes: readonly { name: string }[];
}): DataLossList {
  return {
    rust: [],
    cloud: [
      { kind: "environment", name: input.cloudName },
      ...input.services.map((service) => ({
        kind: "service",
        name: `${input.cloudName}/${service.name}`,
      })),
      ...input.volumes.map((volume) => ({
        kind: "volume",
        name: volume.name,
      })),
    ],
  };
}

export function projectCloudRow(input: {
  organizationSlug: string;
  projectSlug: string;
}): DataLossList {
  return {
    rust: [],
    cloud: [
      {
        kind: "project",
        name: `${input.organizationSlug}/${input.projectSlug}`,
      },
    ],
  };
}

export function organizationCloudRow(organizationSlug: string): DataLossList {
  return {
    rust: [],
    cloud: [{ kind: "organization", name: organizationSlug }],
  };
}

export function teardownIsBusy(status: TeardownAttemptStatus) {
  return status === "pending" || status === "running";
}

export function teardownIsRetryable(status: TeardownAttemptStatus) {
  return status === "pending";
}

export function teardownIsTerminal(status: TeardownAttemptStatus) {
  return !teardownIsBusy(status);
}

export type TeardownRetryPlan =
  | { kind: "resend" }
  | { kind: "conflict" };

export function retryPlanForAttempt(status: TeardownAttemptStatus): TeardownRetryPlan {
  switch (status) {
    case "pending":
      return { kind: "resend" };
    case "partial":
    case "failed":
    case "cancelled":
    case "running":
    case "completed":
      return { kind: "conflict" };
    default: {
      const exhaustive: never = status;
      return exhaustive;
    }
  }
}
