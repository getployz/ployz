import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { environment as schemaEnvironment } from "#/modules/project/tables";
import {
  DestructiveVolumeConflict,
  DestructiveVolumeEvidenceInvalid,
  DestructiveVolumePersistenceFailure,
} from "#/modules/operations/destructive-volume-errors";
import { Effect } from "effect";
import {
  completeDestructiveVolumeAttempt,
  finalizeUnassociatedDestructiveVolumeAttempt,
  recordDestructiveVolumeEvent,
} from "#/modules/operations/destructive-volume-attempt.repository";
import type {
  DestructiveVolumeAttemptRecord,
} from "#/modules/operations/destructive-volume-attempt-persistence.server";
import {
  isDestructiveVolumeTerminalEvidenceEvent,
  projectDestructiveVolumeTerminalAttemptEvent,
  projectDestructiveVolumeWorkflowAttempt,
  type DestructiveVolumeAttemptEvent,
  type DestructiveVolumeWorkflowAttempt,
} from "#/modules/operations/destructive-volume-attempt";
import {
  closeCoreOperationWatch,
  type CoreOperationWatchClose,
  type PersistedOperationEvent,
} from "#/modules/operations/core-operation-evidence.server";
import {
  destructiveVolumeEventFromPersisted,
  type DestructiveVolumeEvidenceEvent,
} from "#/modules/operations/destructive-volume-operation-evidence";
import { Database, type DatabaseService } from "#/server/database.server";
import {
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
} from "#/modules/operations/tables";

export type DestructiveVolumeWorkflowContext = {
  attempt: DestructiveVolumeWorkflowAttempt;
  organizationId: string;
};

export function loadDestructiveVolumeWorkflowContext(
  input: { attemptId: string },
) {
  return Effect.gen(function* () {
    const database = yield* Database;
    const [row] = yield* workflowContextBase(database)
      .where(eq(schemaDestructiveVolumeAttempt.id, input.attemptId))
      .limit(1);
    return normalizeWorkflowContext(row ?? null);
  }).pipe(
    Effect.mapError(workflowError),
  );
}

export function loadDestructiveVolumeCancellationContext(
  input: { inngestRunId: string },
) {
  return Effect.gen(function* () {
    const database = yield* Database;
    const [row] = yield* workflowContextBase(database)
      .where(eq(schemaDestructiveVolumeAttempt.inngestRunId, input.inngestRunId))
      .limit(1);
    return normalizeWorkflowContext(row ?? null);
  }).pipe(
    Effect.mapError(workflowError),
  );
}

type CloseDeps<RClose, RRecord, RComplete> = {
  closeWatch(input: {
    organizationId: string;
    coreOperationId: string;
    state: "cloud_timeout" | "cloud_cancelled";
    expectedInngestRunId: string;
  }): Effect.Effect<CoreOperationWatchClose, unknown, RClose>;
  parseTerminal(event: PersistedOperationEvent): DestructiveVolumeEvidenceEvent | null;
  record(input: {
    attemptId: string;
    event: DestructiveVolumeAttemptEvent;
    now: Date;
  }): Effect.Effect<
    { state: "recorded" | "replayed"; attempt: DestructiveVolumeAttemptRecord },
    unknown,
    RRecord
  >;
  complete(input: {
    attemptId: string;
    operationId: string;
    now: Date;
  }): Effect.Effect<
    { state: "recorded" | "replayed"; attempt: DestructiveVolumeAttemptRecord },
    unknown,
    RComplete
  >;
};

export function closeDestructiveVolumeWatchAndReconcileWithDeps<
  RClose,
  RRecord,
  RComplete,
>(
  input: {
    attemptId: string;
    organizationId: string;
    operationId: string;
    startSequence: string;
    expectedInngestRunId: string;
    state: "cloud_timeout" | "cloud_cancelled";
    now: Date;
  },
  deps: CloseDeps<RClose, RRecord, RComplete>,
) {
  return Effect.gen(function* () {
  const closed = yield* deps.closeWatch({
      organizationId: input.organizationId,
      coreOperationId: input.operationId,
      state: input.state,
      expectedInngestRunId: input.expectedInngestRunId,
    });
  if (closed.state === "missing") {
    return yield* new DestructiveVolumeConflict({ message: "Associated destructive volume watch is missing." });
  }
  if (closed.state === "core_terminal") {
    const terminal = deps.parseTerminal(closed.terminalEvent);
    if (!terminal || !isDestructiveVolumeTerminalEvidenceEvent(terminal)) {
      return yield* new DestructiveVolumeEvidenceInvalid({
        message: "Core destructive volume terminal evidence is invalid.",
      });
    }
    const recorded =
      terminal.event === "volume_remove_completed"
        ? yield* deps.complete({
            attemptId: input.attemptId,
            operationId: input.operationId,
            now: input.now,
          })
        : yield* deps.record({
            attemptId: input.attemptId,
            event: projectDestructiveVolumeTerminalAttemptEvent(
              input.operationId,
              terminal,
            ),
            now: input.now,
          });
    return { state: "core_terminal" as const, attempt: recorded.attempt };
  }
  const observationState = closed.observationState;
  const event: DestructiveVolumeAttemptEvent = observationState === "cloud_cancelled"
    ? { event: "cloud_cancelled", operationId: input.operationId }
    : {
        event: "cloud_timeout",
        operationId: input.operationId,
        startSequence: input.startSequence,
      };
  const recorded = yield* deps.record({ attemptId: input.attemptId, event, now: input.now });
  return {
    state: closed.state === "already_closed" ? "replayed" as const : "cloud_terminal" as const,
    attempt: recorded.attempt,
  };
  });
}

export function closeDestructiveVolumeWatchAndReconcile(input: {
  attemptId: string;
  organizationId: string;
  operationId: string;
  expectedInngestRunId: string;
  state: "cloud_timeout" | "cloud_cancelled";
  now?: Date;
}) {
  return Effect.gen(function* () {
    const database = yield* Database;
    const context = yield* loadDestructiveVolumeWorkflowContext({
      attemptId: input.attemptId,
    });
    if (
      !context ||
      context.organizationId !== input.organizationId ||
      context.attempt.state !== "owned_associated" ||
      context.attempt.operationId !== input.operationId ||
      context.attempt.inngestRunId !== input.expectedInngestRunId
    ) {
      return yield* new DestructiveVolumeConflict({
        message: "Destructive volume watch authority conflicts.",
      });
    }
    const startSequence = context.attempt.startSequence;
    return yield* closeDestructiveVolumeWatchAndReconcileWithDeps(
        {
          ...input,
          startSequence,
          now: input.now ?? new Date(),
        },
        {
          closeWatch: (values) => closeCoreOperationWatch(values, database),
          parseTerminal: destructiveVolumeEventFromPersisted,
          record: recordDestructiveVolumeEvent,
          complete: completeDestructiveVolumeAttempt,
        },
      );
  }).pipe(
    Effect.mapError(workflowError),
  );
}

export { finalizeUnassociatedDestructiveVolumeAttempt };

function workflowContextBase(database: DatabaseService) {
  return database.drizzle.select({
    attempt: schemaDestructiveVolumeAttempt,
    organizationId: schemaEnvironment.organizationId,
  }).from(schemaDestructiveVolumeAttempt)
    .leftJoin(
      schemaEnvironmentDeployment,
      eq(schemaEnvironmentDeployment.id, schemaDestructiveVolumeAttempt.environmentDeploymentId),
    )
    .innerJoin(
      schemaEnvironment,
      eq(schemaEnvironment.id, schemaEnvironmentDeployment.environmentId),
    );
}

function normalizeWorkflowContext(
  row: {
    attempt: typeof schemaDestructiveVolumeAttempt.$inferSelect;
    organizationId: string;
  } | null,
): DestructiveVolumeWorkflowContext | null {
  if (!row) return null;
  return {
    organizationId: row.organizationId,
    attempt: projectDestructiveVolumeWorkflowAttempt(row.attempt),
  };
}

function workflowError(cause: unknown) {
  return (
    cause instanceof DestructiveVolumeConflict ||
    cause instanceof DestructiveVolumeEvidenceInvalid ||
    cause instanceof DestructiveVolumePersistenceFailure
      ? cause
      : new DestructiveVolumePersistenceFailure({ cause })
  );
}
