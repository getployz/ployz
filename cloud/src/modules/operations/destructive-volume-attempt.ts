import { Effect } from "effect";
import { DestructiveVolumeConflict } from "#/modules/operations/destructive-volume-errors";
import { areDeepEqual } from "#/utils/schema-path";
import type { DestructiveVolumeEvidence } from "./destructive-volume-evidence";
import type { DestructiveVolumeEvidenceEvent } from "./destructive-volume-operation-evidence";

export const DESTRUCTIVE_VOLUME_ATTEMPT_DISPOSITIONS = [
  "active",
  "accepted",
  "completed",
  "partial",
  "core_terminal",
  "cloud_timeout",
  "cloud_cancelled",
  "failed",
] as const;

export type DestructiveVolumeAttemptDisposition =
  (typeof DESTRUCTIVE_VOLUME_ATTEMPT_DISPOSITIONS)[number];

export type DestructiveVolumeRetryMode =
  | "new_removal"
  | "reobserve_operation"
  | "reconcile_tombstone";

export function destructiveVolumeRetryModeForAttempt(attempt: {
  disposition: DestructiveVolumeAttemptDisposition;
  operationId: string | null;
  startSequence: string | null;
  failure?: AttemptFailure | null;
}): DestructiveVolumeRetryMode | null {
  const associated = Boolean(attempt.operationId && attempt.startSequence);
  if (attempt.disposition === "partial" && associated) {
    return "reconcile_tombstone";
  }
  if (
    (attempt.disposition === "cloud_timeout" ||
      attempt.disposition === "cloud_cancelled") &&
    associated
  ) {
    return "reobserve_operation";
  }
  if (
    (attempt.disposition === "failed" &&
      attempt.failure?.code !== "recovery_uncertain") ||
    (attempt.disposition === "cloud_cancelled" &&
      !attempt.operationId &&
      !attempt.startSequence)
  ) {
    return "new_removal";
  }
  return null;
}

export type ReviewedDestructiveVolumeTarget = {
  version: 1;
  resourceId: string;
  namespaceId: string;
  volumeName: string;
  machineId: string;
};

export type ReviewedDestructiveVolumeEvidence = {
  version: 1;
  fingerprint: string;
  reviewedAt: string;
  evidence: DestructiveVolumeEvidence;
};

export type DestructiveVolumeWorkflowAttempt =
  | ({ state: "new" } & DestructiveVolumeWorkflowAttemptIdentity)
  | ({ state: "unclaimed_associated" } &
      DestructiveVolumeWorkflowAttemptIdentity &
      AcceptedState)
  | ({ state: "owned_unassociated" } &
      DestructiveVolumeWorkflowAttemptIdentity &
      WorkflowOwnership)
  | ({ state: "owned_associated" } &
      DestructiveVolumeWorkflowAttemptIdentity &
      AcceptedState &
      WorkflowOwnership)
  | {
      state: "terminal";
      id: string;
      disposition: Exclude<
        DestructiveVolumeAttemptDisposition,
        "active" | "accepted"
      >;
    };

type DestructiveVolumeWorkflowAttemptIdentity = {
  id: string;
  target: ReviewedDestructiveVolumeTarget;
  evidence: ReviewedDestructiveVolumeEvidence;
};

type WorkflowOwnership = {
  inngestRunId: string;
  deadlineAt: Date | string;
};

export type DestructiveVolumeWorkflowAttemptSource =
  DestructiveVolumeWorkflowAttemptIdentity & {
    disposition: DestructiveVolumeAttemptDisposition;
    operationId: string | null;
    startSequence: string | null;
    inngestRunId: string | null;
    deadlineAt: Date | string | null;
  };

export function projectDestructiveVolumeWorkflowAttempt(
  attempt: DestructiveVolumeWorkflowAttemptSource,
): DestructiveVolumeWorkflowAttempt {
  const identity = {
    id: attempt.id,
    target: attempt.target,
    evidence: attempt.evidence,
  };
  if (attempt.disposition === "active") {
    if (attempt.operationId || attempt.startSequence) {
      throw new Error("Active destructive volume attempt has Core association evidence.");
    }
    if (!attempt.inngestRunId && !attempt.deadlineAt) {
      return { state: "new", ...identity };
    }
    if (attempt.inngestRunId && attempt.deadlineAt) {
      return {
        state: "owned_unassociated",
        ...identity,
        inngestRunId: attempt.inngestRunId,
        deadlineAt: attempt.deadlineAt,
      };
    }
    throw new Error("Active destructive volume attempt has incomplete workflow ownership.");
  }
  if (attempt.disposition === "accepted") {
    if (!attempt.operationId || !attempt.startSequence) {
      throw new Error("Accepted destructive volume attempt has incomplete Core evidence.");
    }
    const association = {
      operationId: attempt.operationId,
      startSequence: attempt.startSequence,
    };
    if (!attempt.inngestRunId && !attempt.deadlineAt) {
      return {
        state: "unclaimed_associated",
        ...identity,
        ...association,
      };
    }
    if (attempt.inngestRunId && attempt.deadlineAt) {
      return {
        state: "owned_associated",
        ...identity,
        ...association,
        inngestRunId: attempt.inngestRunId,
        deadlineAt: attempt.deadlineAt,
      };
    }
    throw new Error("Accepted destructive volume attempt has incomplete workflow ownership.");
  }
  return {
    state: "terminal",
    id: attempt.id,
    disposition: attempt.disposition,
  };
}

export function sameReviewedDestructiveVolumeIdentity(
  left: {
    target: ReviewedDestructiveVolumeTarget;
    evidence: ReviewedDestructiveVolumeEvidence;
  },
  right: {
    target: ReviewedDestructiveVolumeTarget;
    evidence: ReviewedDestructiveVolumeEvidence;
  },
) {
  return (
    left.evidence.fingerprint === right.evidence.fingerprint &&
    areDeepEqual(left.target, right.target) &&
    areDeepEqual(left.evidence, right.evidence)
  );
}

type AcceptedState = {
  operationId: string;
  startSequence: string;
};

type UnassociatedCancellation = {
  association: "unassociated";
};

type AssociatedCancellation = AcceptedState & {
  association: "associated";
};

type UnassociatedCancellationEvent = {
  event: "cloud_cancelled";
  operationId?: never;
};

type AssociatedCancellationEvent = {
  event: "cloud_cancelled";
  operationId: string;
};

export type DestructiveVolumeAttemptState =
  | { disposition: "active" }
  | ({ disposition: "accepted" } & AcceptedState)
  | ({ disposition: "completed" } & AcceptedState)
  | ({ disposition: "partial"; failure: AttemptFailure } & AcceptedState)
  | ({ disposition: "core_terminal"; terminalEvent: unknown } & AcceptedState)
  | ({ disposition: "cloud_timeout" } & AcceptedState)
  | ({ disposition: "cloud_cancelled" } &
      (UnassociatedCancellation | AssociatedCancellation))
  | { disposition: "failed"; failure: AttemptFailure };

export type AttemptFailure = { code: string; message: string };

export type DestructiveVolumeAttemptEvent =
  | ({ event: "core_accepted" } & AcceptedState)
  | { event: "core_completed"; operationId: string }
  | {
      event: "core_terminal";
      operationId: string;
      terminalEvent: Exclude<
        DestructiveVolumeEvidenceEvent,
        { event: "volume_remove_completed" }
      >;
    }
  | ({ event: "cloud_timeout" } & AcceptedState)
  | UnassociatedCancellationEvent
  | AssociatedCancellationEvent
  | { event: "submission_failed"; failure: AttemptFailure }
  | {
      event: "reconciliation_failed";
      operationId: string;
      code: string;
      message: string;
    };

export const normalizeDestructiveVolumeAttemptEvent = Effect.fn(
  "DestructiveVolume.normalizeAttemptEvent",
)(function* (
  attempt: { readonly disposition: DestructiveVolumeAttemptDisposition },
  event: DestructiveVolumeAttemptEvent,
) {
  if (event.event !== "cloud_cancelled") return event;
  if (attempt.disposition === "accepted") {
    if (!event.operationId) {
      return yield* new DestructiveVolumeConflict({
        message:
          "Accepted destructive volume cancellation is missing Core evidence.",
      });
    }
    return event;
  }
  if (event.operationId) {
    return yield* new DestructiveVolumeConflict({
      message: "Unassociated destructive volume cancellation has Core evidence.",
    });
  }
  return event;
});

export function applyDestructiveVolumeAttemptEvent(
  state: DestructiveVolumeAttemptState,
  event: DestructiveVolumeAttemptEvent,
): DestructiveVolumeAttemptState {
  if (state.disposition === "active") {
    switch (event.event) {
      case "core_accepted":
        return { disposition: "accepted", ...acceptedIdentity(event) };
      case "cloud_cancelled":
        if (event.operationId) {
          throw new Error("Volume removal has no accepted Core operation.");
        }
        return { disposition: "cloud_cancelled", association: "unassociated" };
      case "submission_failed":
        return { disposition: "failed", failure: event.failure };
      case "core_completed":
      case "core_terminal":
      case "cloud_timeout":
      case "reconciliation_failed":
        throw new Error("Volume removal has no accepted Core operation.");
    }
  }
  if (state.disposition !== "accepted") {
    throw new Error("Destructive volume attempt is already terminal.");
  }
  assertOperation(state.operationId, event);
  switch (event.event) {
    case "core_completed":
      return { disposition: "completed", ...acceptedIdentity(state) };
    case "core_terminal":
      return {
        disposition: "core_terminal",
        ...acceptedIdentity(state),
        terminalEvent: event.terminalEvent,
      };
    case "cloud_timeout":
      return { disposition: "cloud_timeout", ...acceptedIdentity(state) };
    case "cloud_cancelled":
      if (!event.operationId) {
        throw new Error("Accepted volume removal cancellation is missing Core evidence.");
      }
      return {
        disposition: "cloud_cancelled",
        association: "associated",
        ...acceptedIdentity(state),
      };
    case "reconciliation_failed":
      return {
        disposition: "partial",
        ...acceptedIdentity(state),
        failure: { code: event.code, message: event.message },
      };
    case "core_accepted":
      if (
        state.operationId === event.operationId &&
        state.startSequence === event.startSequence
      ) {
        return state;
      }
      throw new Error("Volume removal already has different Core evidence.");
    case "submission_failed":
      throw new Error("An accepted volume removal cannot become a submission failure.");
  }
}

export function isTerminalDestructiveVolumeAttemptDisposition(
  disposition: DestructiveVolumeAttemptDisposition,
) {
  switch (disposition) {
    case "active":
    case "accepted":
      return false;
    case "completed":
    case "partial":
    case "core_terminal":
    case "cloud_timeout":
    case "cloud_cancelled":
    case "failed":
      return true;
  }
}

export type DestructiveVolumeTerminalEvidenceEvent = Extract<
  DestructiveVolumeEvidenceEvent,
  | { event: "volume_remove_completed" }
  | { event: "volume_remove_failed" }
  | { event: "operation_interrupted" }
  | { event: "cancelled" }
>;

export function isDestructiveVolumeTerminalEvidenceEvent(
  event: DestructiveVolumeEvidenceEvent,
): event is DestructiveVolumeTerminalEvidenceEvent {
  switch (event.event) {
    case "volume_remove_submitted":
    case "volume_remove_running":
      return false;
    case "volume_remove_completed":
    case "volume_remove_failed":
    case "operation_interrupted":
    case "cancelled":
      return true;
  }
}

export function projectDestructiveVolumeTerminalAttemptEvent(
  operationId: string,
  event: DestructiveVolumeTerminalEvidenceEvent,
): DestructiveVolumeAttemptEvent {
  return event.event === "volume_remove_completed"
    ? { event: "core_completed", operationId }
    : { event: "core_terminal", operationId, terminalEvent: event };
}

function acceptedIdentity(input: AcceptedState) {
  return {
    operationId: input.operationId,
    startSequence: input.startSequence,
  };
}

function assertOperation(
  operationId: string,
  event: DestructiveVolumeAttemptEvent,
) {
  if ("operationId" in event && event.operationId !== operationId) {
    throw new Error("Volume removal evidence belongs to a different Core operation.");
  }
}
