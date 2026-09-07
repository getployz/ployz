import "@tanstack/react-start/server-only";
import { and, eq, sql } from "drizzle-orm";
import {
  coreOperationWatch as schemaCoreOperationWatch,
  coreOperationEvent as schemaCoreOperationEvent,
} from "#/modules/operations/tables";
import type { JsonObject } from "#/db/tables";
import type {
  CoreOperationObservationState,
  CoreOperationWatchCursorState,
} from "#/modules/operations/tables";
import {
  CoreOperationEvidencePersistenceFailure,
  CoreOperationWatchConflict,
} from "#/modules/operations/destructive-volume-errors";
import { Effect } from "effect";
import type { DatabaseService } from "#/server/database.server";

export type PersistedOperationEvent = {
  sequence: string;
  eventType: string;
  payload: JsonObject;
};

export type CoreOperationWatchClose =
  | {
      state: "closed";
      observationState: "cloud_timeout" | "cloud_cancelled";
      observationDetail: JsonObject | null;
    }
  | { state: "missing" }
  | { state: "core_terminal"; terminalEvent: PersistedOperationEvent }
  | {
      state: "already_closed";
      observationState: "cloud_timeout" | "cloud_cancelled";
      observationDetail: JsonObject | null;
    };

export type CoreOperationEvidencePage = {
  events: Array<{
    sequence: string;
    eventType: string;
    schemaVersion: number;
    payload: JsonObject;
    createdAt: Date;
  }>;
  hasMore: boolean;
  nextSequence: string | null;
  cursorState: CoreOperationWatchCursorState;
  observationState: CoreOperationObservationState;
  observationDetail: JsonObject | null;
  deadlineAt: Date;
};

export const closeCoreOperationWatch = Effect.fn("Operations.closeWatch")(
function* (input: {
  organizationId: string;
  coreOperationId: string;
  state: "cloud_timeout" | "cloud_cancelled";
  expectedInngestRunId?: string;
  observationDetail?: JsonObject;
}, database: DatabaseService) {
  const updateResult = yield* database.drizzle
    .update(schemaCoreOperationWatch)
    .set({
      observationState: input.state,
      observationDetail: input.observationDetail ?? null,
      terminalAt: new Date(),
      updatedAt: new Date(),
    })
    .where(
      and(
        eq(schemaCoreOperationWatch.organizationId, input.organizationId),
        eq(schemaCoreOperationWatch.operationId, input.coreOperationId),
        eq(schemaCoreOperationWatch.observationState, "active"),
        ...(input.expectedInngestRunId
          ? [
              eq(
                schemaCoreOperationWatch.inngestRunId,
                input.expectedInngestRunId,
              ),
            ]
          : []),
      ),
    )
    .returning({
      id: schemaCoreOperationWatch.id,
      observationDetail: schemaCoreOperationWatch.observationDetail,
    })
    .pipe(
      Effect.mapError(
        (cause) =>
          new CoreOperationEvidencePersistenceFailure({ cause }),
      ),
    );
  if (updateResult.length > 0) {
    return {
      state: "closed" as const,
      observationState: input.state,
      observationDetail: updateResult[0]?.observationDetail ?? null,
    };
  }

  const [watch] = yield* database.drizzle
    .select({
      id: schemaCoreOperationWatch.id,
      organizationId: schemaCoreOperationWatch.organizationId,
      operationId: schemaCoreOperationWatch.operationId,
      expectedKind: schemaCoreOperationWatch.expectedKind,
      startSequence: schemaCoreOperationWatch.startSequence,
      nextSequence: schemaCoreOperationWatch.nextSequence,
      cursorState: schemaCoreOperationWatch.cursorState,
      observationState: schemaCoreOperationWatch.observationState,
      observationDetail: schemaCoreOperationWatch.observationDetail,
      inngestRunId: schemaCoreOperationWatch.inngestRunId,
      deadlineAt: schemaCoreOperationWatch.deadlineAt,
    })
    .from(schemaCoreOperationWatch)
    .where(
      and(
        eq(schemaCoreOperationWatch.organizationId, input.organizationId),
        eq(schemaCoreOperationWatch.operationId, input.coreOperationId),
      ),
    )
    .limit(1)
    .pipe(
      Effect.mapError(
        (cause) =>
          new CoreOperationEvidencePersistenceFailure({ cause }),
      ),
    );
  if (!watch) return { state: "missing" as const };
  if (
    input.expectedInngestRunId &&
    watch.inngestRunId !== input.expectedInngestRunId
  ) {
    return yield* new CoreOperationWatchConflict({
      message: "Core operation watch owner conflicts.",
    });
  }
  if (
    watch.observationState === "core_terminal" ||
    watch.cursorState === "terminal"
  ) {
    const [terminalEvent] = yield* database.drizzle
      .select({
        sequence: schemaCoreOperationEvent.sequence,
        eventType: schemaCoreOperationEvent.eventType,
        payload: schemaCoreOperationEvent.payload,
      })
      .from(schemaCoreOperationEvent)
      .where(eq(schemaCoreOperationEvent.watchId, watch.id))
      .orderBy(sql`${schemaCoreOperationEvent.sequence}::numeric desc`)
      .limit(1)
      .pipe(
        Effect.mapError(
          (cause) =>
            new CoreOperationEvidencePersistenceFailure({ cause }),
        ),
      );
    if (!terminalEvent) {
      return yield* new CoreOperationWatchConflict({
          message:
            "Core operation watch is terminal without terminal evidence.",
        });
    }
    return { state: "core_terminal" as const, terminalEvent };
  }
  if (
    watch.observationState !== "cloud_timeout" &&
    watch.observationState !== "cloud_cancelled"
  ) {
    return yield* new CoreOperationWatchConflict({
        message: "Core operation watch has an invalid terminal disposition.",
      });
  }
  return {
    state: "already_closed" as const,
    observationState: watch.observationState,
    observationDetail: watch.observationDetail,
  };
});

export function listCoreOperationEvidencePageEffect(
  input: {
    organizationId: string;
    coreOperationId: string;
    afterSequence?: string;
    limit: number;
  },
  database: DatabaseService["drizzle"],
) {
  return Effect.gen(function* () {
    const [watch] = yield* database
      .select({
        id: schemaCoreOperationWatch.id,
        cursorState: schemaCoreOperationWatch.cursorState,
        observationState: schemaCoreOperationWatch.observationState,
        observationDetail: schemaCoreOperationWatch.observationDetail,
        deadlineAt: schemaCoreOperationWatch.deadlineAt,
      })
      .from(schemaCoreOperationWatch)
      .where(
        and(
          eq(schemaCoreOperationWatch.organizationId, input.organizationId),
          eq(schemaCoreOperationWatch.operationId, input.coreOperationId),
        ),
      )
      .limit(1);
    if (!watch) return null;

    const rows = yield* database
      .select({
        sequence: schemaCoreOperationEvent.sequence,
        eventType: schemaCoreOperationEvent.eventType,
        schemaVersion: schemaCoreOperationEvent.schemaVersion,
        payload: schemaCoreOperationEvent.payload,
        createdAt: schemaCoreOperationEvent.createdAt,
      })
      .from(schemaCoreOperationEvent)
      .where(
        and(
          eq(schemaCoreOperationEvent.watchId, watch.id),
          ...(input.afterSequence
            ? [
                sql`${schemaCoreOperationEvent.sequence}::numeric > ${input.afterSequence}::numeric`,
              ]
            : []),
        ),
      )
      .orderBy(sql`${schemaCoreOperationEvent.sequence}::numeric asc`)
      .limit(input.limit + 1);
    const hasMore = rows.length > input.limit;
    return {
      events: rows.slice(0, input.limit),
      hasMore,
      nextSequence: hasMore ? (rows[input.limit - 1]?.sequence ?? null) : null,
      cursorState: watch.cursorState,
      observationState: watch.observationState,
      observationDetail: watch.observationDetail,
      deadlineAt: watch.deadlineAt,
    } satisfies CoreOperationEvidencePage;
  }).pipe(
    Effect.mapError(
      (cause) =>
        new CoreOperationEvidencePersistenceFailure({ cause }),
    ),
  );
}
