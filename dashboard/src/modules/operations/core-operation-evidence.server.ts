import "@tanstack/react-start/server-only";
import { and, eq, sql } from "drizzle-orm";
import {
  coreOperationWatch as schemaCoreOperationWatch,
  coreOperationEvent as schemaCoreOperationEvent,
} from "#/modules/operations/tables";
import type { JsonObject } from "#/db/tables";
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
