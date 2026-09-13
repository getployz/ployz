import "@tanstack/react-start/server-only";

import { and, desc, eq, isNull, type SQL } from "drizzle-orm";
import { Effect } from "effect";
import type {
  VolumeRemoveAttemptStatus,
  VolumeRemoveOutcome,
  VolumeRemoveVolume,
} from "#/modules/runtime/volume-removal";
import { volumeRemoveAttempt as schemaVolumeRemoveAttempt } from "#/modules/runtime/tables";
import {
  Database,
  type DatabaseService,
  isUniqueViolation,
} from "#/server/database.server";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
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
  organizationId: string | SQL;
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

export const stageVolumeRemoveAttempt = Effect.fn(
  "VolumeRemovalRepository.stage",
)(function* (input: {
  organizationId: string | SQL;
  requestedByUserId: string;
  environmentId: string;
  environmentDeploymentId: string;
  environmentResourceId: string;
  volumes: readonly VolumeRemoveVolume[];
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
      environmentDeploymentId: input.environmentDeploymentId,
      environmentResourceId: input.environmentResourceId,
      volumes: [...input.volumes],
      status: "awaiting_deployment",
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

export function releaseVolumeRemoveAttemptsForAppliedDeploymentInTransaction(
  tx: DatabaseService["drizzle"],
  environmentDeploymentId: string,
) {
  return Effect.gen(function* () {
    const [deployment] = yield* tx
      .select({ status: schemaEnvironmentDeployment.status })
      .from(schemaEnvironmentDeployment)
      .where(eq(schemaEnvironmentDeployment.id, environmentDeploymentId))
      .limit(1);
    if (deployment?.status !== "applied") {
      return yield* new Conflict({
        message:
          "Volume removal attempts can be released only by their applied deployment.",
      });
    }
    return yield* tx
      .update(schemaVolumeRemoveAttempt)
      .set({ status: "pending", updatedAt: new Date() })
      .where(
        and(
          eq(
            schemaVolumeRemoveAttempt.environmentDeploymentId,
            environmentDeploymentId,
          ),
          eq(schemaVolumeRemoveAttempt.status, "awaiting_deployment"),
        ),
      )
      .returning({ id: schemaVolumeRemoveAttempt.id });
  });
}

export function failAwaitingVolumeRemoveAttemptsForDeploymentInTransaction(
  tx: DatabaseService["drizzle"],
  input: {
    environmentDeploymentId: string;
    deploymentDisposition: "failed" | "cancelled";
    now: Date;
  },
) {
  return Effect.gen(function* () {
    const [deployment] = yield* tx
      .select({ status: schemaEnvironmentDeployment.status })
      .from(schemaEnvironmentDeployment)
      .where(eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId))
      .limit(1);
    if (deployment?.status !== input.deploymentDisposition) {
      return yield* new Conflict({
        message:
          "Volume removal deployment terminalization conflicts with deployment state.",
      });
    }
    return yield* tx
      .update(schemaVolumeRemoveAttempt)
      .set({
        status: input.deploymentDisposition,
        failureMessage: `Deployment ${input.deploymentDisposition} before the approved volume removal was submitted.`,
        terminalAt: input.now,
        updatedAt: input.now,
      })
      .where(
        and(
          eq(
            schemaVolumeRemoveAttempt.environmentDeploymentId,
            input.environmentDeploymentId,
          ),
          eq(schemaVolumeRemoveAttempt.status, "awaiting_deployment"),
        ),
      )
      .returning({ id: schemaVolumeRemoveAttempt.id });
  });
}

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

export const beginVolumeRemoveAttempt = Effect.fn(
  "VolumeRemovalRepository.begin",
)(function* (input: { attemptId: string; inngestRunId: string; now?: Date }) {
  const now = input.now ?? new Date();
  const database = yield* Database;
  return yield* database.transaction(
    Effect.gen(function* () {
      const transaction = yield* Database;
      const rows = yield* transaction.drizzle
        .select()
        .from(schemaVolumeRemoveAttempt)
        .where(eq(schemaVolumeRemoveAttempt.id, input.attemptId))
        .for("update")
        .limit(1);
      const attempt = rows[0];
      if (
        !attempt ||
        attempt.status !== "running" ||
        attempt.inngestRunId !== input.inngestRunId
      ) {
        return yield* new Conflict({
          message: "Volume remove lost workflow ownership before submission.",
        });
      }
      if (attempt.startedAt !== null) {
        const terminalRows = yield* transaction.drizzle
          .update(schemaVolumeRemoveAttempt)
          .set({
            status: "unknown",
            failureMessage:
              "Volume removal may have reached Ployz before Cloud lost the outcome. Retry it explicitly after reviewing the volume state.",
            terminalAt: now,
            updatedAt: now,
          })
          .where(eq(schemaVolumeRemoveAttempt.id, attempt.id))
          .returning();
        const terminal = terminalRows[0];
        if (!terminal) {
          return yield* new Conflict({
            message: "Volume remove unknown outcome was not persisted.",
          });
        }
        return { kind: "unknown" as const, attempt: terminal };
      }
      const startedRows = yield* transaction.drizzle
        .update(schemaVolumeRemoveAttempt)
        .set({ startedAt: now, updatedAt: now })
        .where(eq(schemaVolumeRemoveAttempt.id, attempt.id))
        .returning();
      const started = startedRows[0];
      if (!started) {
        return yield* new Conflict({
          message: "Volume remove submission marker was not persisted.",
        });
      }
      return { kind: "started" as const, attempt: started };
    }),
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

export const markVolumeRemoveAttemptUnknown = Effect.fn(
  "VolumeRemovalRepository.markUnknown",
)(function* (input: {
  attemptId: string;
  inngestRunId: string;
  failureMessage: string;
  now?: Date;
}) {
  const now = input.now ?? new Date();
  const database = yield* Database;
  return yield* database.transaction(
    Effect.gen(function* () {
      const transaction = yield* Database;
      const rows = yield* transaction.drizzle
        .select()
        .from(schemaVolumeRemoveAttempt)
        .where(eq(schemaVolumeRemoveAttempt.id, input.attemptId))
        .for("update")
        .limit(1);
      const attempt = rows[0];
      if (
        !attempt ||
        attempt.status !== "running" ||
        attempt.inngestRunId !== input.inngestRunId ||
        attempt.startedAt === null
      ) {
        return yield* new Conflict({
          message: "Volume remove unknown outcome lost workflow ownership.",
        });
      }
      const updatedRows = yield* transaction.drizzle
        .update(schemaVolumeRemoveAttempt)
        .set({
          status: "unknown",
          failureMessage: input.failureMessage,
          terminalAt: now,
          updatedAt: now,
        })
        .where(eq(schemaVolumeRemoveAttempt.id, input.attemptId))
        .returning();
      const updated = updatedRows[0];
      if (!updated) {
        return yield* new Conflict({
          message: "Volume remove unknown outcome was not persisted.",
        });
      }
      return updated;
    }),
  );
});
