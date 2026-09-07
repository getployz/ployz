import "@tanstack/react-start/server-only";

import { and, desc, eq, isNull } from "drizzle-orm";
import { Effect } from "effect";
import type {
  VolumeRemoveAttemptStatus,
  VolumeRemoveOutcome,
  VolumeRemoveVolume,
} from "#/modules/runtime/volume-removal";
import { volumeRemoveAttempt as schemaVolumeRemoveAttempt } from "#/modules/runtime/tables";
import { Database, isUniqueViolation } from "#/server/database.server";
import { Conflict } from "#/server/public-error";

export type VolumeRemoveAttempt = typeof schemaVolumeRemoveAttempt.$inferSelect;

function activeVolumeRemoveConflict() {
  return new Conflict({
    message: "A volume remove is already running for this volume.",
  });
}

export const loadVolumeRemoveAttempt = Effect.fn("VolumeRemovalRepository.load")(
  function* (attemptId: string) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select()
      .from(schemaVolumeRemoveAttempt)
      .where(eq(schemaVolumeRemoveAttempt.id, attemptId))
      .limit(1);
    return rows[0] ?? null;
  },
);

export const loadVolumeRemoveAttemptByRun = Effect.fn(
  "VolumeRemovalRepository.loadByRun",
)(function* (inngestRunId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select()
    .from(schemaVolumeRemoveAttempt)
    .where(eq(schemaVolumeRemoveAttempt.inngestRunId, inngestRunId))
    .limit(1);
  return rows[0] ?? null;
});

export const loadLatestVolumeRemoveAttemptForResource = Effect.fn(
  "VolumeRemovalRepository.loadLatestForResource",
)(function* (input: {
  environmentId: string;
  environmentResourceId: string;
}) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select()
    .from(schemaVolumeRemoveAttempt)
    .where(
      and(
        eq(schemaVolumeRemoveAttempt.environmentId, input.environmentId),
        eq(
          schemaVolumeRemoveAttempt.environmentResourceId,
          input.environmentResourceId,
        ),
      ),
    )
    .orderBy(desc(schemaVolumeRemoveAttempt.createdAt))
    .limit(1);
  return rows[0] ?? null;
});

export const insertVolumeRemoveAttempt = Effect.fn(
  "VolumeRemovalRepository.insert",
)(function* (input: {
  organizationId: string;
  requestedByUserId: string;
  environmentId: string;
  environmentResourceId: string;
  volumes: readonly VolumeRemoveVolume[];
  retryOfAttemptId?: string;
  now?: Date;
}) {
  const now = input.now ?? new Date();
  const database = yield* Database;
  const rows = yield* database.drizzle
    .insert(schemaVolumeRemoveAttempt)
    .values({
      organizationId: input.organizationId,
      requestedByUserId: input.requestedByUserId,
      environmentId: input.environmentId,
      environmentResourceId: input.environmentResourceId,
      volumes: [...input.volumes],
      status: "pending",
      retryOfAttemptId: input.retryOfAttemptId,
      createdAt: now,
      updatedAt: now,
    })
    .returning()
    .pipe(Effect.catchIf(isUniqueViolation, activeVolumeRemoveConflict));
  const created = rows[0];
  if (!created) {
    return yield* Effect.die(new Error("Insert returned no row."));
  }
  return created;
});

export const claimVolumeRemoveAttempt = Effect.fn(
  "VolumeRemovalRepository.claim",
)(function* (input: { attemptId: string; inngestRunId: string; now?: Date }) {
  const now = input.now ?? new Date();
  const database = yield* Database;
  return yield* database
    .transaction(
      Effect.gen(function* () {
        const transaction = yield* Database;
        const claimedRows = yield* transaction.drizzle
          .update(schemaVolumeRemoveAttempt)
          .set({
            status: "running",
            inngestRunId: input.inngestRunId,
            startedAt: now,
            updatedAt: now,
          })
          .where(
            and(
              eq(schemaVolumeRemoveAttempt.id, input.attemptId),
              eq(schemaVolumeRemoveAttempt.status, "pending"),
              isNull(schemaVolumeRemoveAttempt.inngestRunId),
            ),
          )
          .returning();
        const claimed = claimedRows[0];
        if (claimed) return { kind: "claimed" as const, attempt: claimed };

        const existingRows = yield* transaction.drizzle
          .select()
          .from(schemaVolumeRemoveAttempt)
          .where(eq(schemaVolumeRemoveAttempt.id, input.attemptId))
          .limit(1);
        const existing = existingRows[0];
        if (existing?.inngestRunId === input.inngestRunId) {
          return { kind: "replayed" as const, attempt: existing };
        }
        return yield* new Conflict({
          message: "Volume remove has another workflow owner.",
        });
      }),
    )
    .pipe(
      Effect.catchIf(isUniqueViolation, () =>
        new Conflict({
          message: "Volume remove has another workflow owner.",
        }),
      ),
    );
});

export const completeVolumeRemoveAttempt = Effect.fn(
  "VolumeRemovalRepository.complete",
)(function* (input: {
  attemptId: string;
  inngestRunId: string;
  status: Extract<
    VolumeRemoveAttemptStatus,
    "completed" | "partial" | "failed" | "cancelled"
  >;
  outcome?: VolumeRemoveOutcome | null;
  failureMessage?: string | null;
  now?: Date;
}) {
  const now = input.now ?? new Date();
  const database = yield* Database;
  return yield* database
    .transaction(
      Effect.gen(function* () {
        const transaction = yield* Database;
        const ownedRows = yield* transaction.drizzle
          .select()
          .from(schemaVolumeRemoveAttempt)
          .where(eq(schemaVolumeRemoveAttempt.id, input.attemptId))
          .for("update")
          .limit(1);
        const owned = ownedRows[0];
        if (
          !owned ||
          owned.status !== "running" ||
          owned.inngestRunId !== input.inngestRunId
        ) {
          return yield* new Conflict({
            message: "Volume remove completion lost workflow ownership.",
          });
        }
        const updatedRows = yield* transaction.drizzle
          .update(schemaVolumeRemoveAttempt)
          .set({
            status: input.status,
            outcome: input.outcome ?? null,
            failureMessage: input.failureMessage ?? null,
            terminalAt: now,
            updatedAt: now,
          })
          .where(eq(schemaVolumeRemoveAttempt.id, input.attemptId))
          .returning();
        const updated = updatedRows[0];
        if (!updated) {
          return yield* new Conflict({
            message: "Volume remove completion was not persisted.",
          });
        }
        return updated;
      }),
    )
    .pipe(
      Effect.catchIf(isUniqueViolation, () =>
        new Conflict({
          message: "Volume remove completion lost workflow ownership.",
        }),
      ),
    );
});
