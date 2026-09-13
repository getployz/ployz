import {
  confirmedVolumeRemove,
  type ConfirmedVolumeRemove,
} from "#/modules/runtime/data-loss-confirm";
import { Schema } from "effect";
import {
  dataLossIdentitySchema,
  type DataLossIdentity,
} from "#/modules/runtime/data-loss-identity";

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const Uuid = Schema.String.check(Schema.isUUID());

export const VolumeResourceInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  environmentId: Uuid,
  resourceId: Uuid,
});
export type VolumeResourceInput = typeof VolumeResourceInput.Type;

export const ConfirmVolumeRemoveInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  environmentId: Uuid,
  resourceId: Uuid,
  identities: Schema.Array(dataLossIdentitySchema),
});
export type ConfirmVolumeRemoveInput = typeof ConfirmVolumeRemoveInput.Type;

export const RetryVolumeRemoveInput = Schema.Struct({
  organizationSlug: NonEmptyString,
  attemptId: Uuid,
});
export type RetryVolumeRemoveInput = typeof RetryVolumeRemoveInput.Type;

export const VOLUME_REMOVE_ATTEMPT_STATUSES = [
  "awaiting_deployment",
  "pending",
  "running",
  "unknown",
  "completed",
  "partial",
  "failed",
  "cancelled",
] as const;

export type VolumeRemoveAttemptStatus =
  (typeof VOLUME_REMOVE_ATTEMPT_STATUSES)[number];

export type VolumeRemoveVolume = ConfirmedVolumeRemove[number];

export type VolumeRemoveFailedVolume = VolumeRemoveVolume & {
  message?: string;
};

export type VolumeRemoveOutcome = {
  destroyed: VolumeRemoveVolume[];
  failed: VolumeRemoveFailedVolume[];
  omitted: VolumeRemoveVolume[];
};

export function volumeIdentityKey(volume: VolumeRemoveVolume): string {
  return `${volume.machine_id}\0${volume.name}`;
}

export function remainingVolumeIdentities(
  requested: readonly VolumeRemoveVolume[],
  destroyed: readonly VolumeRemoveVolume[],
): VolumeRemoveVolume[] {
  const gone = new Set(destroyed.map(volumeIdentityKey));
  return requested.filter((volume) => !gone.has(volumeIdentityKey(volume)));
}

function identitySet(volumes: readonly VolumeRemoveVolume[]): Set<string> {
  return new Set(volumes.map(volumeIdentityKey));
}

function requestedIn(
  requested: readonly VolumeRemoveVolume[],
  listed: readonly VolumeRemoveVolume[],
): VolumeRemoveVolume[] {
  const keys = identitySet(listed);
  return requested.filter((volume) => keys.has(volumeIdentityKey(volume)));
}

/** Completed only when every requested identity was destroyed and none failed or omitted. */
export function volumeRemoveStatusFromOutcome(
  requested: readonly VolumeRemoveVolume[],
  outcome: VolumeRemoveOutcome,
): "completed" | "partial" {
  const destroyed = requestedIn(requested, outcome.destroyed);
  const failed = requestedIn(requested, outcome.failed);
  const omitted = requestedIn(requested, outcome.omitted);
  if (
    destroyed.length === requested.length &&
    failed.length === 0 &&
    omitted.length === 0
  ) {
    return "completed";
  }
  return "partial";
}

export type VolumeRemoveConfirmFilter = {
  volumes: ConfirmedVolumeRemove;
  error: "empty" | "name_mismatch" | null;
};

export function volumesConfirmedForPhysicalName(
  identities: readonly DataLossIdentity[],
  physicalName: string,
): VolumeRemoveConfirmFilter {
  const volumes = confirmedVolumeRemove({
    rust: [...identities],
    cloud: [],
  });
  if (volumes.some((volume) => volume.name !== physicalName)) {
    return { volumes, error: "name_mismatch" };
  }
  if (volumes.length === 0) {
    return { volumes, error: "empty" };
  }
  return { volumes, error: null };
}

export function volumeRemoveIsBusy(status: VolumeRemoveAttemptStatus) {
  return (
    status === "awaiting_deployment" ||
    status === "pending" ||
    status === "running"
  );
}

export function volumeRemoveIsRetryable(status: VolumeRemoveAttemptStatus) {
  return (
    status === "pending" ||
    status === "unknown" ||
    status === "partial" ||
    status === "failed" ||
    status === "cancelled"
  );
}

export function volumeRemoveIsTerminal(status: VolumeRemoveAttemptStatus) {
  return !volumeRemoveIsBusy(status);
}

export type VolumeRemoveRetryPlan =
  | { kind: "resend" }
  | { kind: "retry"; volumes: VolumeRemoveVolume[] }
  | { kind: "conflict" };

export function retryVolumesForAttempt(attempt: {
  status: VolumeRemoveAttemptStatus;
  volumes: readonly VolumeRemoveVolume[];
  outcome: VolumeRemoveOutcome | null;
}): VolumeRemoveRetryPlan {
  switch (attempt.status) {
    case "awaiting_deployment":
      return { kind: "conflict" };
    case "pending":
      return { kind: "resend" };
    case "partial":
      return {
        kind: "retry",
        volumes: remainingVolumeIdentities(
          attempt.volumes,
          attempt.outcome?.destroyed ?? [],
        ),
      };
    case "unknown":
    case "failed":
    case "cancelled":
      return { kind: "retry", volumes: [...attempt.volumes] };
    case "running":
    case "completed":
      return { kind: "conflict" };
    default: {
      const exhaustive: never = attempt.status;
      return exhaustive;
    }
  }
}
