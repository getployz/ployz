import "@tanstack/react-start/server-only";

import { and, desc, eq, isNull } from "drizzle-orm";
import { Effect } from "effect";
import type { DataLossIdentity } from "#/modules/runtime/data-loss-identity";
import {
  parseTeardownTargets,
  type TeardownAttemptStatus,
  type TeardownOutcome,
  type TeardownScope,
  type TeardownTargets,
} from "#/modules/runtime/teardown";
import { teardownAttempt as schemaTeardownAttempt } from "#/modules/runtime/tables";
import { Database, isUniqueViolation } from "#/server/database.server";
import { Conflict } from "#/server/public-error";

export type TeardownAttempt = typeof schemaTeardownAttempt.$inferSelect;

function parsedAttempt(attempt: TeardownAttempt): TeardownAttempt {
  return { ...attempt, targets: parseTeardownTargets(attempt.targets) };
}

function activeTeardownConflict() {
  return new Conflict({
    message: "A teardown is already running for this target.",
  });
}

export const loadTeardownAttempt = Effect.fn("TeardownRepository.load")(
  function* (attemptId: string) {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select()
      .from(schemaTeardownAttempt)
      .where(eq(schemaTeardownAttempt.id, attemptId))
      .limit(1);
    return rows[0] ? parsedAttempt(rows[0]) : null;
  },
);

export const loadTeardownAttemptByRun = Effect.fn(
  "TeardownRepository.loadByRun",
)(function* (inngestRunId: string) {
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select()
    .from(schemaTeardownAttempt)
    .where(eq(schemaTeardownAttempt.inngestRunId, inngestRunId))
    .limit(1);
  return rows[0] ? parsedAttempt(rows[0]) : null;
});

export const loadLatestTeardownAttemptForScope = Effect.fn(
  "TeardownRepository.loadLatestForScope",
)(function* (input: {
  organizationId: string;
  scope: TeardownScope;
  projectId?: string | null;
  environmentId?: string | null;
}) {
  const scopeFilter =
    input.scope === "environment"
      ? and(
          eq(schemaTeardownAttempt.scope, "environment"),
          eq(schemaTeardownAttempt.environmentId, input.environmentId ?? ""),
        )
      : input.scope === "project"
        ? and(
            eq(schemaTeardownAttempt.scope, "project"),
            eq(schemaTeardownAttempt.projectId, input.projectId ?? ""),
          )
        : and(
            eq(schemaTeardownAttempt.scope, "organization"),
            eq(schemaTeardownAttempt.organizationId, input.organizationId),
          );
  const database = yield* Database;
  const rows = yield* database.drizzle
    .select()
    .from(schemaTeardownAttempt)
    .where(
      and(
        eq(schemaTeardownAttempt.organizationId, input.organizationId),
        scopeFilter,
      ),
    )
    .orderBy(desc(schemaTeardownAttempt.createdAt))
    .limit(1);
  return rows[0] ? parsedAttempt(rows[0]) : null;
});

export const insertTeardownAttempt = Effect.fn("TeardownRepository.insert")(
  function* (input: {
    organizationId: string;
    requestedByUserId: string;
    projectId?: string | null;
    environmentId?: string | null;
    scope: TeardownScope;
    confirmDataLoss: readonly DataLossIdentity[];
    targets: TeardownTargets;
    retryOfAttemptId?: string;
    now?: Date;
  }) {
    const now = input.now ?? new Date();
    const database = yield* Database;
    const rows = yield* database.drizzle
      .insert(schemaTeardownAttempt)
      .values({
        organizationId: input.organizationId,
        requestedByUserId: input.requestedByUserId,
        projectId: input.projectId ?? null,
        environmentId: input.environmentId ?? null,
        retryOfAttemptId: input.retryOfAttemptId,
        scope: input.scope,
        confirmDataLoss: [...input.confirmDataLoss],
        targets: input.targets,
        status: "pending",
        createdAt: now,
        updatedAt: now,
      })
      .returning()
      .pipe(Effect.catchIf(isUniqueViolation, activeTeardownConflict));
    const created = rows[0];
    if (!created) {
      return yield* Effect.die(new Error("Insert returned no row."));
    }
    return parsedAttempt(created);
  },
);

export const claimTeardownAttempt = Effect.fn("TeardownRepository.claim")(
  function* (input: { attemptId: string; inngestRunId: string; now?: Date }) {
    const now = input.now ?? new Date();
    const database = yield* Database;
    return yield* database
      .transaction(
        Effect.gen(function* () {
          const transaction = yield* Database;
          const claimedRows = yield* transaction.drizzle
            .update(schemaTeardownAttempt)
            .set({
              status: "running",
              inngestRunId: input.inngestRunId,
              startedAt: now,
              updatedAt: now,
            })
            .where(
              and(
                eq(schemaTeardownAttempt.id, input.attemptId),
                eq(schemaTeardownAttempt.status, "pending"),
                isNull(schemaTeardownAttempt.inngestRunId),
              ),
            )
            .returning();
          const claimed = claimedRows[0];
          if (claimed) {
            return { kind: "claimed" as const, attempt: parsedAttempt(claimed) };
          }
          const existingRows = yield* transaction.drizzle
            .select()
            .from(schemaTeardownAttempt)
            .where(eq(schemaTeardownAttempt.id, input.attemptId))
            .limit(1);
          const existing = existingRows[0];
          if (existing?.inngestRunId === input.inngestRunId) {
            return { kind: "replayed" as const, attempt: parsedAttempt(existing) };
          }
          return yield* new Conflict({
            message: "Teardown has another workflow owner.",
          });
        }),
      )
      .pipe(Effect.catchIf(isUniqueViolation, () =>
        new Conflict({
          message: "Teardown has another workflow owner.",
        }),
      ));
  },
);

export const completeTeardownAttempt = Effect.fn(
  "TeardownRepository.complete",
)(function* (input: {
  attemptId: string;
  inngestRunId: string;
  status: Extract<
    TeardownAttemptStatus,
    "completed" | "partial" | "failed" | "cancelled"
  >;
  outcome?: TeardownOutcome | null;
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
          .from(schemaTeardownAttempt)
          .where(eq(schemaTeardownAttempt.id, input.attemptId))
          .for("update")
          .limit(1);
        const owned = ownedRows[0];
        if (
          !owned ||
          owned.status !== "running" ||
          owned.inngestRunId !== input.inngestRunId
        ) {
          return yield* new Conflict({
            message: "Teardown completion lost workflow ownership.",
          });
        }
        const updatedRows = yield* transaction.drizzle
          .update(schemaTeardownAttempt)
          .set({
            status: input.status,
            outcome: input.outcome ?? null,
            failureMessage: input.failureMessage ?? null,
            terminalAt: now,
            updatedAt: now,
          })
          .where(eq(schemaTeardownAttempt.id, input.attemptId))
          .returning();
        const updated = updatedRows[0];
        if (!updated) {
          return yield* new Conflict({
            message: "Teardown completion was not persisted.",
          });
        }
        return parsedAttempt(updated);
      }),
    )
    .pipe(
      Effect.catchIf(isUniqueViolation, () =>
        new Conflict({
          message: "Teardown completion lost workflow ownership.",
        }),
      ),
    );
});
