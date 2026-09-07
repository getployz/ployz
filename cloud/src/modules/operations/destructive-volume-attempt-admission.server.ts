import "@tanstack/react-start/server-only";
import { and, eq, isNotNull } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import {
  environmentResource as schemaEnvironmentResource,
} from "#/modules/environment-design/tables";
import {
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
} from "#/modules/operations/tables";
import { organizationIdForDeployment } from "#/db/scope-values.server";
import {
  destructiveVolumeRetryModeForAttempt,
  isTerminalDestructiveVolumeAttemptDisposition,
  sameReviewedDestructiveVolumeIdentity,
  type DestructiveVolumeRetryMode,
} from "#/modules/operations/destructive-volume-attempt";
import {
  assertDestructiveVolumeReplayIdentity,
  assertReviewedDestructiveVolumeIdentity,
  type CreateDestructiveVolumeAttemptInput,
  type DestructiveVolumeAttemptRecord,
  destructiveVolumeRepositoryError,
} from "#/modules/operations/destructive-volume-attempt-persistence.server";
import { DestructiveVolumeConflict } from "#/modules/operations/destructive-volume-errors";
import { Database } from "#/server/database.server";
import { mutationReceiptInTransaction } from "#/server/mutation-receipt.server";
import { areDeepEqual } from "#/utils/schema-path";

export type DestructiveVolumeStagingAuthority = {
  removalDeploymentStatus: string;
  tombstoneCount: number;
  priorAppliedSnapshotCount: number;
};

export function validateDestructiveVolumeStagingAuthority(
  authority: DestructiveVolumeStagingAuthority | null,
): Effect.Effect<void, DestructiveVolumeConflict> {
  if (
    authority?.removalDeploymentStatus !== "queued" ||
    authority.tombstoneCount !== 1 ||
    authority.priorAppliedSnapshotCount !== 1
  ) {
    return new DestructiveVolumeConflict({
      message:
        "Destructive volume staging requires the queued removal deployment, one exact tombstone, and the prior applied deployment snapshot.",
    });
  }
  return Effect.void;
}

type RetryAttemptInput = {
  attemptId: string;
  target: CreateDestructiveVolumeAttemptInput["target"];
  evidence: CreateDestructiveVolumeAttemptInput["evidence"];
};

export type RetryDestructiveVolumeAttemptOutcome =
  (
    | {
        state: "created" | "replayed";
        mode: "new_removal";
        attempt: DestructiveVolumeAttemptRecord;
      }
    | {
        state: "created" | "replayed";
        mode: "reobserve_operation";
        attempt: DestructiveVolumeAttemptRecord;
        operationId: NonNullable<DestructiveVolumeAttemptRecord["operationId"]>;
        startSequence: string;
      }
    | {
        state: "created" | "replayed";
        mode: "reconcile_tombstone";
        attempt: DestructiveVolumeAttemptRecord;
        operationId: NonNullable<DestructiveVolumeAttemptRecord["operationId"]>;
      }
  ) & { txid?: number };

function retryConflict() {
  return new DestructiveVolumeConflict({
    message:
      "Destructive volume retry requires the original applied deployment and exact tombstoned resource.",
  });
}

function validateRetry(
  input: RetryAttemptInput,
  source: {
    attempt: DestructiveVolumeAttemptRecord;
    deploymentStatus: string;
    tombstoneCount: number;
  } | null,
) {
  return Effect.gen(function* () {
    const original = source?.attempt;
    if (
      !source ||
      !original ||
      !isTerminalDestructiveVolumeAttemptDisposition(original.disposition) ||
      source.deploymentStatus !== "applied" ||
      source.tombstoneCount !== 1
    ) {
      return yield* retryConflict();
    }
    yield* assertReviewedDestructiveVolumeIdentity({
      environmentDeploymentId: original.environmentDeploymentId,
      environmentResourceId: original.environmentResourceId,
      target: input.target,
      evidence: input.evidence,
    });
    if (!areDeepEqual(original.target, input.target)) {
      return yield* new DestructiveVolumeConflict({
        message: "Destructive volume retry target changed from the original attempt.",
      });
    }
    const mode = destructiveVolumeRetryModeForAttempt(original);
    if (!mode) return yield* retryConflict();
    if (
      mode !== "new_removal" &&
      !sameReviewedDestructiveVolumeIdentity(original, input)
    ) {
      return yield* new DestructiveVolumeConflict({
        message:
          "Associated destructive volume retry evidence changed from the original review.",
      });
    }
    if (
      mode === "new_removal" &&
      original.disposition !== "failed" &&
      !(
        original.disposition === "cloud_cancelled" &&
        !original.operationId &&
        !original.startSequence
      )
    ) {
      return yield* retryConflict();
    }
    return { original, mode };
  });
}

export const retryOutcome = Effect.fn(
  "DestructiveVolume.retryOutcome",
)(function* (
  state: "created" | "replayed",
  mode: DestructiveVolumeRetryMode,
  source: DestructiveVolumeAttemptRecord,
  attempt: DestructiveVolumeAttemptRecord,
) {
  if (mode === "new_removal") {
    return { state, mode, attempt, txid: undefined };
  }
  if (!attempt.operationId || !attempt.startSequence) {
    return yield* new DestructiveVolumeConflict({
      message: "Associated destructive volume retry lost its Core identity.",
    });
  }
  if (
    attempt.operationId !== source.operationId ||
    attempt.startSequence !== source.startSequence
  ) {
    return yield* new DestructiveVolumeConflict({
      message:
        "Associated destructive volume retry conflicts with the original Core operation.",
    });
  }
  return mode === "reobserve_operation"
    ? {
        state,
        mode,
        attempt,
        operationId: attempt.operationId,
        startSequence: attempt.startSequence,
        txid: undefined,
      }
    : {
        state,
        mode,
        attempt,
        operationId: attempt.operationId,
        txid: undefined,
      };
});

export const retryDestructiveVolumeAttempt = Effect.fn(
  "Operations.retryDestructiveVolumeAttempt",
)(function* (input: RetryAttemptInput) {
  const database = yield* Database;
  return yield* database
    .transaction(
      Effect.gen(function* () {
        const tx = (yield* Database).drizzle;
        const [row] = yield* tx
          .select({
            attempt: schemaDestructiveVolumeAttempt,
            deploymentStatus: schemaEnvironmentDeployment.status,
            deploymentEnvironmentId: schemaEnvironmentDeployment.environmentId,
          })
          .from(schemaDestructiveVolumeAttempt)
          .innerJoin(
            schemaEnvironmentDeployment,
            eq(
              schemaEnvironmentDeployment.id,
              schemaDestructiveVolumeAttempt.environmentDeploymentId,
            ),
          )
          .where(eq(schemaDestructiveVolumeAttempt.id, input.attemptId))
          .limit(1);
        const tombstones = row
          ? yield* tx
              .select({ id: schemaEnvironmentResource.id })
              .from(schemaEnvironmentResource)
              .where(
                and(
                  eq(
                    schemaEnvironmentResource.id,
                    row.attempt.environmentResourceId,
                  ),
                  eq(
                    schemaEnvironmentResource.environmentId,
                    row.deploymentEnvironmentId,
                  ),
                  eq(schemaEnvironmentResource.implementationType, "volume"),
                  isNotNull(schemaEnvironmentResource.deletedAt),
                ),
              )
          : [];
        const { original, mode } = yield* validateRetry(
          input,
          row
            ? {
                attempt: row.attempt,
                deploymentStatus: row.deploymentStatus,
                tombstoneCount: tombstones.length,
              }
            : null,
        );
        const loadRetry = () =>
          tx
            .select()
            .from(schemaDestructiveVolumeAttempt)
            .where(eq(schemaDestructiveVolumeAttempt.retryOfAttemptId, original.id))
            .limit(1)
            .pipe(Effect.map(([attempt]) => attempt ?? null));
        const existing = yield* loadRetry();
        if (existing) {
          yield* assertDestructiveVolumeReplayIdentity(existing, {
            environmentDeploymentId: original.environmentDeploymentId,
            environmentResourceId: original.environmentResourceId,
            target: input.target,
            evidence: input.evidence,
          });
          return yield* retryOutcome("replayed", mode, original, existing);
        }
        const associated =
          mode === "new_removal"
            ? {}
            : {
                disposition: "accepted" as const,
                operationId: original.operationId,
                startSequence: original.startSequence,
                acceptedAt: new Date(),
              };
        const [created] = yield* tx
          .insert(schemaDestructiveVolumeAttempt)
          .values({
            organizationId: organizationIdForDeployment(
              original.environmentDeploymentId,
            ),
            environmentDeploymentId: original.environmentDeploymentId,
            environmentResourceId: original.environmentResourceId,
            retryOfAttemptId: original.id,
            target: input.target,
            evidence: input.evidence,
            evidenceFingerprint: input.evidence.fingerprint,
            ...associated,
          })
          .onConflictDoNothing()
          .returning();
        const attempt = created ?? (yield* loadRetry());
        if (!attempt) {
          return yield* new DestructiveVolumeConflict({
            message: "Destructive volume retry lost its evidence race.",
          });
        }
        const outcome = yield* retryOutcome(
          created ? "created" : "replayed",
          mode,
          original,
          attempt,
        );
        if (!created) return outcome;
        const receipt = yield* mutationReceiptInTransaction(outcome);
        return { ...outcome, txid: receipt.txid };
      }),
    )
    .pipe(
      Effect.mapError(
        destructiveVolumeRepositoryError,
      ),
    );
});
