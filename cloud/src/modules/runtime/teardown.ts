import {
  confirmedVolumeRemove,
  type DataLossList,
} from "#/modules/runtime/data-loss-confirm";
import {
  dataLossIdentitySchema,
  type DataLossIdentity,
} from "#/modules/runtime/data-loss-identity";
import {
  remainingVolumeIdentities,
  volumeRemoveStatusFromOutcome,
  type VolumeRemoveOutcome,
  type VolumeRemoveVolume,
} from "#/modules/runtime/volume-removal";
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

export type TeardownVolumeOwnership = {
  namespace: string;
  machine: string;
  name: string;
};

export type TeardownEnvironmentTarget = {
  environmentId: string;
  projectId: string;
  namespace: string;
  cloudName: string;
  identities: DataLossIdentity[];
};

export type TeardownRuntimeMembership = "verified" | "unknown" | "untouched";

export type TeardownRuntimeOutcome = "verified_zero" | "unknown" | "untouched";

export type TeardownTargets = {
  environments: TeardownEnvironmentTarget[];
  machines: string[];
  revokePairing: boolean;
  runtimeMembership: TeardownRuntimeMembership;
};

/** Old attempt JSON may omit membership; treat that as env/project untouched. */
export function parseTeardownTargets(targets: {
  environments: TeardownEnvironmentTarget[];
  machines: string[];
  revokePairing: boolean;
  runtimeMembership?: TeardownRuntimeMembership;
}): TeardownTargets {
  return {
    environments: targets.environments,
    machines: targets.machines,
    revokePairing: targets.revokePairing,
    runtimeMembership: targets.runtimeMembership ?? "untouched",
  };
}

export type TeardownOutcome =
  | {
      rustMustRevokePairing: false;
      runtimeMembership: TeardownRuntimeOutcome;
    }
  | {
      rustMustRevokePairing: true;
      runtimeMembership: "unknown";
    };

export type TeardownClusterView =
  | { kind: "no_cluster" }
  | { kind: "unreachable" }
  | { kind: "live"; machines: readonly string[] };

export type TeardownRuntimePlan =
  | {
      kind: "ok";
      machines: string[];
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
      machines: [],
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
      machines: [],
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
        machines: [],
        revokePairing: false,
        runtimeMembership: "untouched",
      };
    case "live":
      return {
        kind: "ok",
        machines: [...input.cluster.machines],
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
  outcome: TeardownOutcome | null,
): string {
  const membership = outcome?.runtimeMembership ?? "untouched";
  switch (membership) {
    case "unknown":
      return outcome?.rustMustRevokePairing
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
): TeardownOutcome {
  if (rustMustRevokePairing) {
    return { rustMustRevokePairing: true, runtimeMembership: "unknown" };
  }
  return {
    rustMustRevokePairing: false,
    runtimeMembership: teardownRuntimeOutcome(membership),
  };
}

/** Inngest retries the same step. Do not persist failed for leftover rust. */
export class TeardownIncompleteError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "TeardownIncompleteError";
  }
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

export function machineCloudRow(machineId: string): DataLossList {
  return {
    rust: [],
    cloud: [{ kind: "machine", name: machineId }],
  };
}

export function identitiesForMachine(
  identities: readonly DataLossIdentity[],
  machineId: string,
): DataLossIdentity[] {
  return identities.filter((identity) => identity.id.machine_id === machineId);
}

function volumeOwnershipKey(machine: string, name: string) {
  return `${machine}\0${name}`;
}

export function identitiesForEnvironment(
  identities: readonly DataLossIdentity[],
  namespace: string,
  ownership: readonly TeardownVolumeOwnership[],
): DataLossIdentity[] {
  const keys = new Set(
    ownership
      .filter((row) => row.namespace === namespace)
      .map((row) => volumeOwnershipKey(row.machine, row.name)),
  );
  return identities.filter(
    (identity) =>
      keys.has(
        volumeOwnershipKey(identity.id.machine_id, identity.id.name),
      ),
  );
}

export function environmentTargetsWithIdentities(input: {
  environments: readonly Omit<TeardownEnvironmentTarget, "identities">[];
  identities: readonly DataLossIdentity[];
  ownership: readonly TeardownVolumeOwnership[];
}): TeardownEnvironmentTarget[] {
  return input.environments.map((environment) => ({
    ...environment,
    identities: identitiesForEnvironment(
      input.identities,
      environment.namespace,
      input.ownership,
    ),
  }));
}

export function leftoverVolumeMessage(
  requested: readonly VolumeRemoveVolume[],
  outcome: VolumeRemoveOutcome,
): string | null {
  const leftover = remainingVolumeIdentities(requested, outcome.destroyed);
  if (
    leftover.length === 0 &&
    volumeRemoveStatusFromOutcome(requested, outcome) === "completed"
  ) {
    return null;
  }
  return `Volume remove left ${leftover.length} identit${leftover.length === 1 ? "y" : "ies"} for Inngest to retry.`;
}

export function teardownIsBusy(status: TeardownAttemptStatus) {
  return status === "pending" || status === "running";
}

export function teardownIsRetryable(status: TeardownAttemptStatus) {
  return (
    status === "pending" ||
    status === "partial" ||
    status === "failed" ||
    status === "cancelled"
  );
}

export function teardownIsTerminal(status: TeardownAttemptStatus) {
  return !teardownIsBusy(status);
}

export type TeardownRetryPlan =
  | { kind: "resend" }
  | { kind: "retry" }
  | { kind: "conflict" };

export function retryPlanForAttempt(status: TeardownAttemptStatus): TeardownRetryPlan {
  switch (status) {
    case "pending":
      return { kind: "resend" };
    case "partial":
    case "failed":
    case "cancelled":
      return { kind: "retry" };
    case "running":
    case "completed":
      return { kind: "conflict" };
    default: {
      const exhaustive: never = status;
      return exhaustive;
    }
  }
}

export function confirmedVolumesForTeardown(
  identities: readonly DataLossIdentity[],
) {
  return confirmedVolumeRemove({ rust: [...identities], cloud: [] });
}
