import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNull } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import {
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
  coreOperationWatch as schemaCoreOperationWatch,
} from "#/modules/operations/tables";
import { environment as schemaEnvironment } from "#/modules/project/tables";
import {
  applyDestructiveVolumeAttemptEvent,
  type DestructiveVolumeAttemptEvent,
  isTerminalDestructiveVolumeAttemptDisposition,
  normalizeDestructiveVolumeAttemptEvent,
} from "#/modules/operations/destructive-volume-attempt";
import {
  type DestructiveVolumeAttemptEventInput,
  destructiveVolumeAttemptFields,
  destructiveVolumeAttemptState,
  destructiveVolumeRepositoryError,
  destructiveVolumeTerminalPayload,
  loadDestructiveVolumeAttempt,
  loadDestructiveVolumeAttemptForOrganization,
} from "#/modules/operations/destructive-volume-attempt-persistence.server";
import { DestructiveVolumeConflict } from "#/modules/operations/destructive-volume-errors";
import { Database } from "#/server/database.server";

const WATCH_TIMEOUT_MS = 10 * 60_000;

function repositoryTransaction<A, E, R>(program: Effect.Effect<A, E, R | Database>) {
  return Effect.gen(function* () {
    const database = yield* Database;
    return yield* database.transaction(program);
  }).pipe(Effect.mapError(destructiveVolumeRepositoryError));
}

export const claimDestructiveVolumeRun = Effect.fn(
  "Operations.claimDestructiveVolumeRun",
)(function* (input: {
  attemptId: string;
  inngestRunId: string;
  now?: Date;
}) {
  const now = input.now ?? new Date();
  return yield* repositoryTransaction(
    Effect.gen(function* () {
      const database = yield* Database;
      const [attempt] = yield* database.drizzle
        .update(schemaDestructiveVolumeAttempt)
        .set({
          inngestRunId: input.inngestRunId,
          deadlineAt: new Date(now.getTime() + WATCH_TIMEOUT_MS),
          updatedAt: now,
        })
        .where(
          and(
            eq(schemaDestructiveVolumeAttempt.id, input.attemptId),
            inArray(schemaDestructiveVolumeAttempt.disposition, ["active", "accepted"]),
            isNull(schemaDestructiveVolumeAttempt.inngestRunId),
          ),
        )
        .returning();
      if (attempt?.disposition === "accepted" && attempt.operationId) {
        const [owner] = yield* database.drizzle
          .select({ organizationId: schemaEnvironment.organizationId })
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
            eq(schemaEnvironment.id, schemaEnvironmentDeployment.environmentId),
          )
          .where(eq(schemaDestructiveVolumeAttempt.id, attempt.id))
          .limit(1);
        if (!owner) {
          return yield* new DestructiveVolumeConflict({
            message: "Destructive volume retry lost its organization owner.",
          });
        }
        const [watch] = yield* database.drizzle
          .update(schemaCoreOperationWatch)
          .set({
            observationState: "active",
            observationDetail: null,
            inngestRunId: input.inngestRunId,
            deadlineAt: new Date(now.getTime() + WATCH_TIMEOUT_MS),
            terminalAt: null,
            updatedAt: now,
          })
          .where(
            and(
              eq(schemaCoreOperationWatch.organizationId, owner.organizationId),
              eq(schemaCoreOperationWatch.operationId, attempt.operationId),
              inArray(schemaCoreOperationWatch.observationState, [
                "cloud_timeout",
                "cloud_cancelled",
              ]),
            ),
          )
          .returning({ id: schemaCoreOperationWatch.id });
        if (!watch) {
          return yield* new DestructiveVolumeConflict({
            message:
              "Destructive volume retry requires its closed original Core watch.",
          });
        }
      }
      if (attempt) return { state: "claimed" as const, attempt };
      const existing = yield* loadDestructiveVolumeAttempt(input.attemptId);
      if (existing?.inngestRunId === input.inngestRunId) {
        return { state: "replayed" as const, attempt: existing };
      }
      return yield* new DestructiveVolumeConflict({
        message: "Destructive volume attempt has another workflow owner.",
      });
    }),
  );
});

export const attachDestructiveVolumeOperation = Effect.fn(
  "Operations.attachDestructiveVolumeOperation",
)(function* (input: {
  organizationId: string;
  attemptId: string;
  operationId: string;
  startSequence: string;
  inngestRunId: string;
}) {
  return yield* repositoryTransaction(
    Effect.gen(function* () {
      const database = yield* Database;
      const attempt = yield* loadDestructiveVolumeAttemptForOrganization(
        input.attemptId,
        input.organizationId,
      );
      if (
        attempt?.disposition === "active" &&
        attempt.inngestRunId === input.inngestRunId &&
        attempt.deadlineAt
      ) {
        yield* database.drizzle
          .insert(schemaCoreOperationWatch)
          .values({
            organizationId: input.organizationId,
            operationId: input.operationId,
            expectedKind: "volume_remove",
            startSequence: input.startSequence,
            nextSequence: input.startSequence,
            cursorState: "more",
            observationState: "active",
            inngestRunId: input.inngestRunId,
            deadlineAt: attempt.deadlineAt,
          })
          .onConflictDoNothing({
            target: [
              schemaCoreOperationWatch.organizationId,
              schemaCoreOperationWatch.operationId,
            ],
          });
        const [watch] = yield* database.drizzle
          .select({
            expectedKind: schemaCoreOperationWatch.expectedKind,
            startSequence: schemaCoreOperationWatch.startSequence,
            inngestRunId: schemaCoreOperationWatch.inngestRunId,
          })
          .from(schemaCoreOperationWatch)
          .where(
            and(
              eq(schemaCoreOperationWatch.organizationId, input.organizationId),
              eq(schemaCoreOperationWatch.operationId, input.operationId),
            ),
          )
          .limit(1);
        if (
          watch?.expectedKind !== "volume_remove" ||
          watch.startSequence !== input.startSequence ||
          watch.inngestRunId !== input.inngestRunId
        ) {
          return yield* new DestructiveVolumeConflict({
            message:
              "Destructive volume watch conflicts with exact operation evidence.",
          });
        }
        const now = new Date();
        const [updated] = yield* database.drizzle
          .update(schemaDestructiveVolumeAttempt)
          .set({
            disposition: "accepted",
            operationId: input.operationId,
            startSequence: input.startSequence,
            acceptedAt: now,
            updatedAt: now,
          })
          .where(
            and(
              eq(schemaDestructiveVolumeAttempt.id, input.attemptId),
              eq(schemaDestructiveVolumeAttempt.disposition, "active"),
              isNull(schemaDestructiveVolumeAttempt.operationId),
              eq(schemaDestructiveVolumeAttempt.inngestRunId, input.inngestRunId),
            ),
          )
          .returning();
        if (updated) return { state: "attached" as const, attempt: updated };
      }
      const existing = yield* loadDestructiveVolumeAttempt(input.attemptId);
      if (
        existing?.disposition === "accepted" &&
        existing.operationId === input.operationId &&
        existing.startSequence === input.startSequence &&
        existing.inngestRunId === input.inngestRunId
      ) {
        return { state: "replayed" as const, attempt: existing };
      }
      return yield* new DestructiveVolumeConflict({
        message: "Destructive volume operation association conflicts with evidence.",
      });
    }),
  );
});

export const recordDestructiveVolumeEvent = Effect.fn(
  "Operations.recordDestructiveVolumeEvent",
)(function* (input: {
  attemptId: string;
  event: DestructiveVolumeAttemptEventInput;
  now?: Date;
}) {
  return yield* repositoryTransaction(
    Effect.gen(function* () {
      const database = yield* Database;
      const attempt = yield* loadDestructiveVolumeAttempt(input.attemptId);
      if (!attempt) {
        return yield* new DestructiveVolumeConflict({ message: "Destructive volume attempt is missing." });
      }
      const event = yield* normalizeDestructiveVolumeAttemptEvent(
        attempt,
        input.event,
      );
      if (isTerminalDestructiveVolumeAttemptDisposition(attempt.disposition)) {
        if (
          JSON.stringify(attempt.terminalEvent) ===
          JSON.stringify(destructiveVolumeTerminalPayload(event))
        ) {
          return { state: "replayed" as const, attempt };
        }
        return yield* new DestructiveVolumeConflict({
          message:
            "Destructive volume terminal evidence conflicts with the recorded result.",
        });
      }
      const next = applyDestructiveVolumeAttemptEvent(
        destructiveVolumeAttemptState(attempt),
        event,
      );
      const [updated] = yield* database.drizzle
        .update(schemaDestructiveVolumeAttempt)
        .set(destructiveVolumeAttemptFields(next, event, input.now ?? new Date()))
        .where(
          and(
            eq(schemaDestructiveVolumeAttempt.id, attempt.id),
            eq(schemaDestructiveVolumeAttempt.disposition, attempt.disposition),
            eq(schemaDestructiveVolumeAttempt.updatedAt, attempt.updatedAt),
          ),
        )
        .returning();
      if (updated) return { state: "recorded" as const, attempt: updated };
      const raced = yield* loadDestructiveVolumeAttempt(input.attemptId);
      if (raced && isTerminalDestructiveVolumeAttemptDisposition(raced.disposition)) {
        return { state: "replayed" as const, attempt: raced };
      }
      return yield* new DestructiveVolumeConflict({
        message: "Destructive volume evidence update lost its state race.",
      });
    }),
  );
});

export const finalizeUnassociatedDestructiveVolumeAttempt = Effect.fn(
  "Operations.finalizeUnassociatedDestructiveVolumeAttempt",
)(function* (input: {
  attemptId: string;
  organizationId: string;
  expectedInngestRunId: string;
  event: "submission_failed" | "cloud_cancelled";
  now?: Date;
  message: string;
  failureCode?: string;
}) {
  return yield* repositoryTransaction(
    Effect.gen(function* () {
      const database = yield* Database;
      const attempt = yield* loadDestructiveVolumeAttemptForOrganization(
        input.attemptId,
        input.organizationId,
      );
      if (!attempt) {
        return yield* new DestructiveVolumeConflict({ message: "Destructive volume attempt is missing." });
      }
      if (isTerminalDestructiveVolumeAttemptDisposition(attempt.disposition)) {
        return { state: "replayed" as const, attempt };
      }
      if (
        attempt.disposition !== "active" ||
        attempt.operationId ||
        attempt.inngestRunId !== input.expectedInngestRunId
      ) {
        return yield* new DestructiveVolumeConflict({
          message: "Unassociated destructive volume workflow ownership conflicts.",
        });
      }
      const event: DestructiveVolumeAttemptEvent =
        input.event === "submission_failed"
          ? {
              event: "submission_failed",
              failure: {
                code: input.failureCode ?? "retry_exhausted",
                message: input.message,
              },
            }
          : { event: "cloud_cancelled" };
      const [updated] = yield* database.drizzle
        .update(schemaDestructiveVolumeAttempt)
        .set(
          destructiveVolumeAttemptFields(
            applyDestructiveVolumeAttemptEvent({ disposition: "active" }, event),
            event,
            input.now ?? new Date(),
          ),
        )
        .where(
          and(
            eq(schemaDestructiveVolumeAttempt.id, input.attemptId),
            eq(schemaDestructiveVolumeAttempt.disposition, "active"),
            eq(schemaDestructiveVolumeAttempt.inngestRunId, input.expectedInngestRunId),
            isNull(schemaDestructiveVolumeAttempt.operationId),
          ),
        )
        .returning();
      if (!updated) {
        return yield* new DestructiveVolumeConflict({
          message:
            "Unassociated destructive volume finalization lost its evidence race.",
        });
      }
      return { state: "recorded" as const, attempt: updated };
    }),
  );
});

export const establishOrConfirmDestructiveVolumeTimeout = Effect.fn(
  "Operations.establishOrConfirmDestructiveVolumeTimeout",
)(function* (input: {
  organizationId: string;
  attemptId: string;
  operationId: string;
  startSequence: string;
  expectedInngestRunId: string;
  now?: Date;
}) {
  const now = input.now ?? new Date();
  const terminalEvent = {
    event: "cloud_timeout",
    operationId: input.operationId,
    startSequence: input.startSequence,
  } as const;
  return yield* repositoryTransaction(
    Effect.gen(function* () {
      const database = yield* Database;
      const attempt = yield* loadDestructiveVolumeAttemptForOrganization(
        input.attemptId,
        input.organizationId,
      );
      if (!attempt) {
        return yield* new DestructiveVolumeConflict({
          message: "Destructive volume timeout authority is missing.",
        });
      }
      if (isTerminalDestructiveVolumeAttemptDisposition(attempt.disposition)) {
        if (
          attempt.disposition === "cloud_timeout" &&
          attempt.operationId === input.operationId &&
          attempt.startSequence === input.startSequence &&
          attempt.inngestRunId === input.expectedInngestRunId &&
          JSON.stringify(attempt.terminalEvent) ===
            JSON.stringify(destructiveVolumeTerminalPayload(terminalEvent))
        ) {
          return { state: "replayed" as const, attempt };
        }
        return yield* new DestructiveVolumeConflict({
          message: "Destructive volume timeout conflicts with terminal evidence.",
        });
      }
      const unassociated =
        attempt.disposition === "active" && !attempt.operationId && !attempt.startSequence;
      const associated =
        attempt.disposition === "accepted" &&
        attempt.operationId === input.operationId &&
        attempt.startSequence === input.startSequence;
      if (
        (!unassociated && !associated) ||
        attempt.inngestRunId !== input.expectedInngestRunId ||
        !attempt.deadlineAt
      ) {
        return yield* new DestructiveVolumeConflict({
          message: "Destructive volume timeout conflicts with exact workflow authority.",
        });
      }
      yield* database.drizzle
        .insert(schemaCoreOperationWatch)
        .values({
          organizationId: input.organizationId,
          operationId: input.operationId,
          expectedKind: "volume_remove",
          startSequence: input.startSequence,
          nextSequence: input.startSequence,
          cursorState: "more",
          observationState: "active",
          inngestRunId: input.expectedInngestRunId,
          deadlineAt: attempt.deadlineAt,
        })
        .onConflictDoNothing({
          target: [
            schemaCoreOperationWatch.organizationId,
            schemaCoreOperationWatch.operationId,
          ],
        });
      const [watch] = yield* database.drizzle
        .select({
          id: schemaCoreOperationWatch.id,
          expectedKind: schemaCoreOperationWatch.expectedKind,
          startSequence: schemaCoreOperationWatch.startSequence,
          inngestRunId: schemaCoreOperationWatch.inngestRunId,
          observationState: schemaCoreOperationWatch.observationState,
        })
        .from(schemaCoreOperationWatch)
        .where(
          and(
            eq(schemaCoreOperationWatch.organizationId, input.organizationId),
            eq(schemaCoreOperationWatch.operationId, input.operationId),
          ),
        )
        .for("update");
      if (
        !watch ||
        watch.expectedKind !== "volume_remove" ||
        watch.startSequence !== input.startSequence ||
        watch.inngestRunId !== input.expectedInngestRunId ||
        (watch.observationState !== "active" &&
          watch.observationState !== "cloud_timeout")
      ) {
        return yield* new DestructiveVolumeConflict({
          message:
            "Destructive volume timeout watch conflicts with exact operation evidence.",
        });
      }
      if (watch.observationState === "active") {
        const closed = yield* database.drizzle
          .update(schemaCoreOperationWatch)
          .set({ observationState: "cloud_timeout", terminalAt: now, updatedAt: now })
          .where(
            and(
              eq(schemaCoreOperationWatch.id, watch.id),
              eq(schemaCoreOperationWatch.organizationId, input.organizationId),
              eq(schemaCoreOperationWatch.operationId, input.operationId),
              eq(schemaCoreOperationWatch.inngestRunId, input.expectedInngestRunId),
              eq(schemaCoreOperationWatch.observationState, "active"),
            ),
          )
          .returning({ id: schemaCoreOperationWatch.id });
        if (closed.length !== 1) {
          return yield* new DestructiveVolumeConflict({
            message: "Destructive volume timeout lost its watch closure race.",
          });
        }
      }
      const [updated] = yield* database.drizzle
        .update(schemaDestructiveVolumeAttempt)
        .set({
          disposition: "cloud_timeout",
          operationId: input.operationId,
          startSequence: input.startSequence,
          acceptedAt: attempt.acceptedAt ?? now,
          terminalEvent: destructiveVolumeTerminalPayload(terminalEvent),
          failure: null,
          terminalAt: now,
          updatedAt: now,
        })
        .where(
          and(
            eq(schemaDestructiveVolumeAttempt.id, input.attemptId),
            eq(schemaDestructiveVolumeAttempt.inngestRunId, input.expectedInngestRunId),
            unassociated
              ? and(
                  eq(schemaDestructiveVolumeAttempt.disposition, "active"),
                  isNull(schemaDestructiveVolumeAttempt.operationId),
                  isNull(schemaDestructiveVolumeAttempt.startSequence),
                )
              : and(
                  eq(schemaDestructiveVolumeAttempt.disposition, "accepted"),
                  eq(schemaDestructiveVolumeAttempt.operationId, input.operationId),
                  eq(schemaDestructiveVolumeAttempt.startSequence, input.startSequence),
                ),
          ),
        )
        .returning();
      if (!updated) {
        return yield* new DestructiveVolumeConflict({
          message: "Destructive volume timeout lost its attempt evidence race.",
        });
      }
      return { state: "recorded" as const, attempt: updated };
    }),
  );
});
