import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { environment as schemaEnvironment } from "#/modules/project/tables";
import type { JsonObject } from "#/db/tables";
import {
  DestructiveVolumeConflict,
  DestructiveVolumeEvidenceInvalid,
  DestructiveVolumePersistenceFailure,
} from "#/modules/operations/destructive-volume-errors";
import { Effect } from "effect";
import {
  type DestructiveVolumeAttemptEvent,
  type DestructiveVolumeAttemptState,
  isTerminalDestructiveVolumeAttemptDisposition,
  sameReviewedDestructiveVolumeIdentity,
  type ReviewedDestructiveVolumeEvidence,
  type ReviewedDestructiveVolumeTarget,
} from "#/modules/operations/destructive-volume-attempt";
import { Database } from "#/server/database.server";
import {
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
} from "#/modules/operations/tables";

export type DestructiveVolumeAttemptRecord =
  Omit<typeof schemaDestructiveVolumeAttempt.$inferSelect, "organizationId">;

export type CreateDestructiveVolumeAttemptInput = {
  environmentDeploymentId: string;
  environmentResourceId: string;
  target: ReviewedDestructiveVolumeTarget;
  evidence: ReviewedDestructiveVolumeEvidence;
};

export type DestructiveVolumeAttemptEventInput = DestructiveVolumeAttemptEvent;
export const loadDestructiveVolumeAttempt = Effect.fn(
  "Operations.loadDestructiveVolumeAttempt",
)(function* (attemptId: string) {
  const database = yield* Database;
  const attempts = yield* database.drizzle
      .select()
      .from(schemaDestructiveVolumeAttempt)
      .where(eq(schemaDestructiveVolumeAttempt.id, attemptId))
      .limit(1);
  return attempts[0] ?? null;
});

export const loadDestructiveVolumeAttemptForOrganization = Effect.fn(
  "Operations.loadDestructiveVolumeAttemptForOrganization",
)(function* (attemptId: string, organizationId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
      .select({ attempt: schemaDestructiveVolumeAttempt })
      .from(schemaDestructiveVolumeAttempt)
      .innerJoin(
        schemaEnvironmentDeployment,
        eq(
          schemaEnvironmentDeployment.id,
          schemaDestructiveVolumeAttempt.environmentDeploymentId,
        ),
      )
      .innerJoin(
        schemaEnvironment,
        eq(
          schemaEnvironment.id,
          schemaEnvironmentDeployment.environmentId,
        ),
      )
      .where(
        and(
          eq(schemaDestructiveVolumeAttempt.id, attemptId),
          eq(schemaEnvironment.organizationId, organizationId),
        ),
      )
      .limit(1);
  return rows[0]?.attempt ?? null;
});

export function assertReviewedDestructiveVolumeIdentity(
  input: CreateDestructiveVolumeAttemptInput,
): Effect.Effect<void, DestructiveVolumeEvidenceInvalid> {
  if (
    input.target.resourceId !== input.environmentResourceId ||
    input.evidence.fingerprint.length === 0 ||
    input.target.namespaceId !== input.evidence.evidence.namespaceId ||
    input.target.volumeName !== input.evidence.evidence.volumeName ||
    input.target.machineId !== input.evidence.evidence.machineId
  ) {
    return new DestructiveVolumeEvidenceInvalid({
      message:
        "Destructive volume target and reviewed testimony do not identify the same volume.",
    });
  }
  return Effect.void;
}

export function assertDestructiveVolumeReplayIdentity(
  attempt: DestructiveVolumeAttemptRecord,
  input: CreateDestructiveVolumeAttemptInput,
): Effect.Effect<void, DestructiveVolumeConflict> {
  if (
    attempt.evidenceFingerprint !== input.evidence.fingerprint ||
    !sameReviewedDestructiveVolumeIdentity(attempt, input)
  ) {
    return new DestructiveVolumeConflict({
      message:
        "An active destructive volume attempt has different reviewed evidence.",
    });
  }
  return Effect.void;
}

export function destructiveVolumeAttemptState(
  attempt: DestructiveVolumeAttemptRecord,
): DestructiveVolumeAttemptState {
  switch (attempt.disposition) {
    case "active":
      return { disposition: "active" };
    case "failed":
      if (!attempt.failure) throw invalidDestructiveVolumeAttempt();
      return { disposition: "failed", failure: attempt.failure };
    case "cloud_cancelled":
      if (attempt.operationId && attempt.startSequence) {
        return {
          disposition: "cloud_cancelled",
          association: "associated",
          operationId: attempt.operationId,
          startSequence: attempt.startSequence,
        };
      }
      return {
        disposition: "cloud_cancelled",
        association: "unassociated",
      };
    case "accepted":
    case "completed":
    case "partial":
    case "core_terminal":
    case "cloud_timeout": {
      if (!attempt.operationId || !attempt.startSequence) {
        throw invalidDestructiveVolumeAttempt();
      }
      const accepted = {
        operationId: attempt.operationId,
        startSequence: attempt.startSequence,
      };
      if (attempt.disposition === "partial") {
        if (!attempt.failure) throw invalidDestructiveVolumeAttempt();
        return {
          disposition: "partial",
          ...accepted,
          failure: attempt.failure,
        };
      }
      if (attempt.disposition === "core_terminal") {
        return {
          disposition: "core_terminal",
          ...accepted,
          terminalEvent: attempt.terminalEvent,
        };
      }
      return { disposition: attempt.disposition, ...accepted };
    }
  }
}

export function destructiveVolumeAttemptFields(
  next: DestructiveVolumeAttemptState,
  event: DestructiveVolumeAttemptEvent,
  now: Date,
) {
  const terminal = isTerminalDestructiveVolumeAttemptDisposition(
    next.disposition,
  );
  return {
    disposition: next.disposition,
    operationId:
      "operationId" in next && next.operationId
        ? next.operationId
        : null,
    startSequence: "startSequence" in next ? next.startSequence : null,
    acceptedAt: next.disposition === "accepted" ? now : undefined,
    terminalEvent: terminal ? destructiveVolumeTerminalPayload(event) : null,
    failure: "failure" in next ? next.failure : null,
    terminalAt: terminal ? now : null,
    updatedAt: now,
  };
}

export function destructiveVolumeTerminalPayload(
  event: DestructiveVolumeAttemptEvent,
) {
  // SAFETY: attempt events are JSON-cloneable objects; persistence stores them as JsonObject.
  return structuredClone(event) as JsonObject;
}

export function invalidDestructiveVolumeAttempt() {
  return new DestructiveVolumeConflict({
    message: "Destructive volume attempt evidence is incomplete.",
  });
}

export function destructiveVolumeRepositoryError(cause: unknown) {
  return cause instanceof DestructiveVolumeConflict ||
    cause instanceof DestructiveVolumeEvidenceInvalid ||
    cause instanceof DestructiveVolumePersistenceFailure
    ? cause
    : new DestructiveVolumePersistenceFailure({ cause });
}
