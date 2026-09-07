import "@tanstack/react-start/server-only";

import { and, eq, isNull } from "drizzle-orm";
import { Effect } from "effect";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import {
  toMachineRemoveAttemptView,
  type MachineRemoveAttemptContext,
  type MachineRemoveCompletion,
} from "#/modules/machines/machine-removal";
import { Database, sqlErrorFrom } from "#/server/database.server";
import { Conflict } from "#/server/public-error";
import { machineRemoveAttempt as schemaMachineRemoveAttempt } from "#/modules/machines/tables";

type Attempt = typeof schemaMachineRemoveAttempt.$inferSelect;

const MACHINE_REMOVE_UNIQUE_CONSTRAINTS = new Set([
  "machine_remove_attempt_inngest_run_uidx",
  "machine_remove_attempt_one_active_org_machine_idx",
]);

export function isMachineRemoveUniqueViolation(cause: unknown) {
  const sqlError = sqlErrorFrom(cause);
  return (
    sqlError !== undefined &&
    sqlError.reason._tag === "UniqueViolation" &&
    MACHINE_REMOVE_UNIQUE_CONSTRAINTS.has(sqlError.reason.constraint)
  );
}

function alreadyInProgress() {
  return new Conflict({
    message: "A machine remove is already in progress.",
  });
}

function toContext(attempt: Attempt): MachineRemoveAttemptContext {
  return {
    id: attempt.id,
    organizationId: attempt.organizationId,
    machineId: attempt.machineId,
    state: attempt.state,
    inngestRunId: attempt.inngestRunId,
    confirmDataLoss: attempt.confirmDataLoss,
  };
}

function completionValues(completion: MachineRemoveCompletion, now: Date) {
  switch (completion.state) {
    case "succeeded":
      return {
        state: "succeeded" as const,
        missingIdentities: null,
        failureCode: null,
        failureMessage: null,
        terminalAt: now,
        updatedAt: now,
      };
    case "failed":
    case "cancelled":
      return {
        state: completion.state,
        missingIdentities: null,
        failureCode: completion.failureCode,
        failureMessage: completion.failureMessage,
        terminalAt: now,
        updatedAt: now,
      };
    case "missing_identities":
      return {
        state: "missing_identities" as const,
        missingIdentities: completion.identities,
        failureCode: null,
        failureMessage: null,
        terminalAt: now,
        updatedAt: now,
      };
    default: {
      const _exhaustive: never = completion;
      throw new Error(`Unhandled machine remove completion: ${_exhaustive}`);
    }
  }
}

export const loadMachineRemoveAttempt = Effect.fn(
  "MachineRemovalRepository.loadAttempt",
)(function* (attemptId: string) {
  const { drizzle } = yield* Database;
  const [attempt] = yield* drizzle
    .select()
    .from(schemaMachineRemoveAttempt)
    .where(eq(schemaMachineRemoveAttempt.id, attemptId))
    .limit(1);
  return attempt ? toContext(attempt) : null;
});

export const loadMachineRemoveAttemptByRun = Effect.fn(
  "MachineRemovalRepository.loadAttemptByRun",
)(function* (inngestRunId: string) {
  const { drizzle } = yield* Database;
  const [attempt] = yield* drizzle
    .select()
    .from(schemaMachineRemoveAttempt)
    .where(eq(schemaMachineRemoveAttempt.inngestRunId, inngestRunId))
    .limit(1);
  return attempt ? toContext(attempt) : null;
});

export const loadAuthorizedMachineRemoveAttempt = Effect.fn(
  "MachineRemovalRepository.loadAuthorized",
)(function* (input: { attemptId: string; organizationId: string }) {
  const { drizzle } = yield* Database;
  const [attempt] = yield* drizzle
    .select()
    .from(schemaMachineRemoveAttempt)
    .where(
      and(
        eq(schemaMachineRemoveAttempt.id, input.attemptId),
        eq(schemaMachineRemoveAttempt.organizationId, input.organizationId),
      ),
    )
    .limit(1);
  return attempt ? toMachineRemoveAttemptView(attempt) : null;
});

export const requestMachineRemoveAttempt = Effect.fn(
  "MachineRemovalRepository.request",
)(function* (input: {
  organizationId: string;
  requestedByUserId: string;
  machineId: string;
  confirmDataLoss: DataLossIdentity[];
}) {
  const { drizzle } = yield* Database;
  const [attempt] = yield* drizzle
    .insert(schemaMachineRemoveAttempt)
    .values({
      organizationId: input.organizationId,
      requestedByUserId: input.requestedByUserId,
      machineId: input.machineId,
      confirmDataLoss: input.confirmDataLoss,
      state: "pending",
    })
    .returning()
    .pipe(
      Effect.catchIf(isMachineRemoveUniqueViolation, () =>
        alreadyInProgress(),
      ),
    );
  if (!attempt) {
    return yield* Effect.die(
      new Error("Machine remove insert returned no row."),
    );
  }
  return toMachineRemoveAttemptView(attempt);
});

export const abandonPendingMachineRemoveAttempt = Effect.fn(
  "MachineRemovalRepository.abandonPending",
)(function* (attemptId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle
    .delete(schemaMachineRemoveAttempt)
    .where(
      and(
        eq(schemaMachineRemoveAttempt.id, attemptId),
        eq(schemaMachineRemoveAttempt.state, "pending"),
        isNull(schemaMachineRemoveAttempt.inngestRunId),
      ),
    );
});

export const claimMachineRemoveAttempt = Effect.fn(
  "MachineRemovalRepository.claim",
)(function* (input: {
  attemptId: string;
  inngestRunId: string;
  now?: Date;
}) {
  const now = input.now ?? new Date();
  const { transaction } = yield* Database;
  return yield* transaction(
    Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [claimed] = yield* drizzle
        .update(schemaMachineRemoveAttempt)
        .set({
          state: "running",
          inngestRunId: input.inngestRunId,
          startedAt: now,
          updatedAt: now,
        })
        .where(
          and(
            eq(schemaMachineRemoveAttempt.id, input.attemptId),
            eq(schemaMachineRemoveAttempt.state, "pending"),
            isNull(schemaMachineRemoveAttempt.inngestRunId),
          ),
        )
        .returning();
      if (claimed) {
        return { kind: "claimed" as const, attempt: toContext(claimed) };
      }

      const [existing] = yield* drizzle
        .select()
        .from(schemaMachineRemoveAttempt)
        .where(eq(schemaMachineRemoveAttempt.id, input.attemptId))
        .limit(1);
      if (existing?.inngestRunId === input.inngestRunId) {
        return { kind: "replayed" as const, attempt: toContext(existing) };
      }
      return yield* new Conflict({
        message: "Machine remove has another workflow owner.",
      });
    }),
  ).pipe(
    Effect.catchIf(isMachineRemoveUniqueViolation, () => alreadyInProgress()),
  );
});

export const completeMachineRemoveAttempt = Effect.fn(
  "MachineRemovalRepository.complete",
)(function* (input: {
  attemptId: string;
  inngestRunId: string;
  completion: MachineRemoveCompletion;
  now?: Date;
}) {
  const now = input.now ?? new Date();
  const { transaction } = yield* Database;
  return yield* transaction(
    Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const [updated] = yield* drizzle
        .update(schemaMachineRemoveAttempt)
        .set(completionValues(input.completion, now))
        .where(
          and(
            eq(schemaMachineRemoveAttempt.id, input.attemptId),
            eq(schemaMachineRemoveAttempt.state, "running"),
            eq(schemaMachineRemoveAttempt.inngestRunId, input.inngestRunId),
          ),
        )
        .returning();
      if (updated) return toContext(updated);

      const [existing] = yield* drizzle
        .select()
        .from(schemaMachineRemoveAttempt)
        .where(eq(schemaMachineRemoveAttempt.id, input.attemptId))
        .limit(1);
      if (
        existing &&
        existing.inngestRunId === input.inngestRunId &&
        existing.state === input.completion.state
      ) {
        return toContext(existing);
      }
      return yield* new Conflict({
        message: "Machine remove completion lost workflow ownership.",
      });
    }),
  );
});
