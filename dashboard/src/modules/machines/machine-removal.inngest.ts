import { Effect, Option, Schema } from "effect";
import {
  createClusterDomainSyncRequestedEvent,
  inngestEventEnvelopeFields,
  inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
  inngestFunctionFailedEnvelopeSchema,
  machineRemoveRequestedEventType,
} from "#/modules/inngest/events";
import type {
  PloyzInngest,
  PloyzStepTools,
} from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { PROCESS_MACHINE_REMOVE_FUNCTION_ID } from "#/modules/inngest/row-backed-workflow-ids";
import {
  cancelMachineRemoveAttemptActivity,
  completeMachineRemoveAttemptActivity,
  failOwnedMachineRemoveAttemptActivity,
  observeLeavingServerActivity,
  prepareMachineRemoveAttemptActivity,
  releaseLeavingLanesActivity,
  releaseServerActivity,
  removeMachineActivity,
} from "#/modules/machines/machine-removal.server";
import { runInngestEffect } from "#/server/run.server";

export { PROCESS_MACHINE_REMOVE_FUNCTION_ID };

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const MachineRemoveRequestedData = Schema.Struct({ attemptId: NonEmptyString });
const MachineRemoveRequestedEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal("machine/remove.requested"),
  data: MachineRemoveRequestedData,
});
const MachineRemoveFailureEnvelope = inngestFunctionFailedEnvelopeSchema(
  MachineRemoveRequestedEnvelope,
);

export const decodeMachineRemoveFailureEvent = Schema.decodeUnknownOption(
  MachineRemoveFailureEnvelope,
);

type StepTools = Pick<PloyzStepTools, "run" | "sendEvent">;

export const MACHINE_REMOVE_RETRIES = 5;
export const isLastTry = (attempt: number) => attempt >= MACHINE_REMOVE_RETRIES;

export async function executeProcessMachineRemove({
  event,
  step,
  runId,
  attempt: stepAttempt,
}: {
  event: { data: unknown };
  step: StepTools;
  runId: string;
  attempt: number;
}) {
  const attemptId = await step.run(
    "normalize-machine-remove-attempt-id",
    () => {
      const decoded = Schema.decodeUnknownOption(MachineRemoveRequestedData)(
        event.data,
        { onExcessProperty: "preserve" },
      );
      return Option.isSome(decoded) ? decoded.value.attemptId : null;
    },
  );
  if (attemptId === null) {
    return { attemptId: null, status: "invalid", skipped: true };
  }

  const prepared = await step.run("claim-machine-remove", () =>
    runInngestEffect(
      prepareMachineRemoveAttemptActivity({
        attemptId,
        inngestRunId: runId,
        now: new Date(),
      }),
    ),
  );
  if (prepared.kind === "missing") {
    return { attemptId, status: "missing", skipped: true };
  }
  if (prepared.kind === "terminal") {
    return {
      attemptId: prepared.attempt.id,
      status: prepared.attempt.state,
      skipped: true,
    };
  }

  const attempt = prepared.attempt;
  // Read before the removal: once the Server is gone the Cluster no longer names it or lists its copies.
  const leaving = await step.run("observe-leaving-server", () =>
    runInngestEffect(observeLeavingServerActivity({
      organizationId: attempt.organizationId,
      machineId: attempt.machineId,
      lastTry: isLastTry(stepAttempt),
    })),
  );
  const sent = await step.run("remove-machine", () =>
    runInngestEffect(Effect.scoped(removeMachineActivity({ ...attempt, inngestRunId: runId }))),
  );
  // The Server reset and its row is still listed. The memoized reply is the proof of the reset: Forget alone finishes,
  // retried while the Server still reads as suspect, and never sends the removal again.
  const removed = sent.kind === "left"
    ? await step.run("forget-machine", () =>
      runInngestEffect(Effect.scoped(removeMachineActivity({ ...attempt, inngestRunId: runId }, { resetWarning: sent.resetWarning }))),
    )
    : sent;

  // The attempt is no longer this run's running attempt: it finished, was cancelled or failed, or another run claimed
  // it. This run asked nothing and leaves the outcome alone.
  if (removed.kind === "superseded") {
    return { attemptId, status: "superseded", skipped: true };
  }

  if (removed.kind === "refused") {
    await step.run("complete-refused", () =>
      runInngestEffect(
        completeMachineRemoveAttemptActivity({
          attemptId,
          inngestRunId: runId,
          completion: { state: "failed", failureCode: removed.failureCode, failureMessage: removed.message },
          now: new Date(),
        }),
      ),
    );
    return { attemptId, status: "failed" as const };
  }

  if (removed.kind === "missing_identities") {
    await step.run("complete-missing-identities", () =>
      runInngestEffect(
        completeMachineRemoveAttemptActivity({
          attemptId,
          inngestRunId: runId,
          completion: {
            state: "missing_identities",
            identities: removed.identities,
          },
          now: new Date(),
        }),
      ),
    );
    return { attemptId, status: "missing_identities" as const };
  }
  await step.run("release-volume-lanes", () =>
    runInngestEffect(releaseLeavingLanesActivity({
      organizationId: attempt.organizationId,
      machineId: attempt.machineId,
      name: leaving.machine.name,
      writes: leaving.writes,
    })),
  );
  // Its own step: a retry after the reset releases again, never resets a Server that's gone.
  const release = await step.run("release-server", () =>
    runInngestEffect(
      releaseServerActivity({
        organizationId: attempt.organizationId,
        machineId: attempt.machineId,
        generation: removed.generation,
        resetWarning: removed.resetWarning,
        noReset: attempt.noReset,
      }),
    ),
  );

  await step.run("complete-succeeded", () =>
    runInngestEffect(
      completeMachineRemoveAttemptActivity({
        attemptId,
        inngestRunId: runId,
        completion: {
          state: "succeeded",
          result: { resetWarning: removed.resetWarning, release },
        },
        now: new Date(),
      }),
    ),
  );
  // The removed Server may have been in the Cluster Domain's apex set.
  await step.sendEvent(
    "request-cluster-domain-sync",
    createClusterDomainSyncRequestedEvent({ organizationId: attempt.organizationId }),
  );
  return { attemptId, status: "succeeded" as const };
}

export async function executeProcessMachineRemoveOnFailure({
  event,
}: {
  event: unknown;
}) {
  const decoded = decodeMachineRemoveFailureEvent(event);
  if (Option.isNone(decoded)) return;
  const attemptId = decoded.value.data.event.data.attemptId;
  const inngestRunId = decoded.value.data.run_id;
  await runInngestEffect(
    failOwnedMachineRemoveAttemptActivity({
      attemptId,
      inngestRunId,
      now: new Date(),
    }),
  );
}

export async function executeCancelMachineRemove({
  event,
  step,
  processFunctionId,
}: {
  event: unknown;
  step: StepTools;
  processFunctionId: string;
}) {
  const decoded = await step.run("decode-machine-remove-cancellation-event", () =>
    decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event),
  );
  if (decoded.data.function_id !== processFunctionId) {
    return { skipped: true };
  }
  const runId = decoded.data.run_id;
  return step.run("cancel-machine-remove", () =>
    runInngestEffect(
      cancelMachineRemoveAttemptActivity({
        inngestRunId: runId,
        now: new Date(),
      }),
    ),
  );
}

export const createProcessMachineRemove = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: PROCESS_MACHINE_REMOVE_FUNCTION_ID,
    retries: MACHINE_REMOVE_RETRIES,
    triggers: [{ event: machineRemoveRequestedEventType }],
    concurrency: [{ key: "event.data.attemptId", limit: 1 }],
    onFailure: async ({ event }) =>
      executeProcessMachineRemoveOnFailure({ event }),
  },
  async ({ event, step, runId, attempt }) =>
    executeProcessMachineRemove({
      event,
      step,
      runId,
      attempt,
    }),
  );

export const createCancelMachineRemove = (inngest: PloyzInngest) => {
  const processFunctionId = createProcessMachineRemove(inngest).id(inngest.id);
  return inngest.createFunction(
  {
    id: "cancel-machine-remove",
    retries: 3,
    triggers: [{ event: inngestFunctionCancelledEventType }],
    concurrency: [{ key: "event.data.run_id", limit: 1 }],
  },
  async ({ event, step }) =>
    executeCancelMachineRemove({ event, step, processFunctionId }),
  );
};
