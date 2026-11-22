import { Effect, Option, Schema } from "effect";
import { inngestEventEnvelopeFields, inngestFunctionCancelledEnvelopeSchema, inngestFunctionCancelledEventType, inngestFunctionFailedEnvelopeSchema } from "#/modules/inngest/events";
import { clusterDomainSyncRequested } from "#/modules/cluster-domain/cluster-domain.events";
import { machineRemoveRequested } from "#/modules/machines/machines.events";
import type { PloyzInngest, StepTools } from "#/modules/inngest/client";
import { eventData } from "#/modules/inngest/define-event";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  cancelMachineRemoveAttemptActivity,
  failOwnedMachineRemoveAttemptActivity,
  observeLeavingServerActivity,
  prepareMachineRemoveAttemptActivity,
  releaseLeavingLanesActivity,
  releaseServerActivity,
  removeMachineActivity,
} from "#/modules/machines/machine-removal.server";
import { completeMachineRemoveAttempt } from "#/modules/machines/machine-removal.repository";
import { observeOrganization } from "#/modules/volume-run/volume-run.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

const MachineRemoveRequestedEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal(machineRemoveRequested.name),
  data: machineRemoveRequested.schema,
});
const MachineRemoveFailureEnvelope = inngestFunctionFailedEnvelopeSchema(
  MachineRemoveRequestedEnvelope,
);

export const decodeMachineRemoveFailureEvent = Schema.decodeUnknownOption(
  MachineRemoveFailureEnvelope,
);

const PROCESS_MACHINE_REMOVE_FUNCTION_ID = "process-machine-remove";
export const MACHINE_REMOVE_RETRIES = 5;

export async function executeProcessMachineRemove({
  event,
  step,
  runId,
  attempt: stepAttempt,
}: {
  event: { data: unknown };
  step: StepTools<"run" | "sendEvent">;
  runId: string;
  attempt: number;
}, runEffect: EffectRunner, observe: typeof observeOrganization) {
  const requested = eventData(machineRemoveRequested)(event);
  if (Option.isNone(requested)) {
    return { attemptId: null, status: "invalid", skipped: true };
  }
  const { attemptId } = requested.value;

  const prepared = await step.run("claim-machine-remove", () =>
    runEffect(
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
    runEffect(observeLeavingServerActivity({
      organizationId: attempt.organizationId,
      machineId: attempt.machineId,
      lastTry: stepAttempt >= MACHINE_REMOVE_RETRIES,
    }, observe)),
  );
  const sent = await step.run("remove-machine", () =>
    runEffect(Effect.scoped(removeMachineActivity({ ...attempt, inngestRunId: runId }))),
  );
  // The Server reset and its row is still listed. The memoized reply is the proof of the reset: Forget alone finishes,
  // retried while the Server still reads as suspect, and never sends the removal again.
  const removed = sent.kind === "left"
    ? await step.run("forget-machine", () =>
      runEffect(Effect.scoped(removeMachineActivity({ ...attempt, inngestRunId: runId }, { resetWarning: sent.resetWarning }))),
    )
    : sent;

  // The attempt is no longer this run's running attempt: it finished, was cancelled or failed, or another run claimed
  // it. This run asked nothing and leaves the outcome alone.
  if (removed.kind === "superseded") {
    return { attemptId, status: "superseded", skipped: true };
  }

  if (removed.kind === "refused") {
    await step.run("complete-refused", () =>
      runEffect(
        completeMachineRemoveAttempt({
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
      runEffect(
        completeMachineRemoveAttempt({
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
    runEffect(releaseLeavingLanesActivity({
      organizationId: attempt.organizationId,
      machineId: attempt.machineId,
      name: leaving.machine.name,
      writes: leaving.writes,
    })),
  );
  const departed = removed.kind === "removed" ? removed.departed : undefined;
  // Its own step: a retry after the reset releases again, never resets a Server that's gone.
  const release = await step.run("release-server", () =>
    runEffect(
      releaseServerActivity({
        organizationId: attempt.organizationId,
        machineId: attempt.machineId,
        generation: removed.generation,
        resetWarning: removed.resetWarning,
        departed,
        noReset: attempt.noReset,
      }),
    ),
  );

  await step.run("complete-succeeded", () =>
    runEffect(
      completeMachineRemoveAttempt({
        attemptId,
        inngestRunId: runId,
        completion: {
          state: "succeeded",
          result: { resetWarning: removed.resetWarning, departed, release },
        },
        now: new Date(),
      }),
    ),
  );
  // The removed Server may have been in the Cluster Domain's apex set.
  await step.sendEvent(
    "request-cluster-domain-sync",
    clusterDomainSyncRequested.create({ organizationId: attempt.organizationId }),
  );
  return { attemptId, status: "succeeded" as const };
}

export async function executeProcessMachineRemoveOnFailure({
  event,
}: {
  event: unknown;
}, runEffect: EffectRunner) {
  const decoded = decodeMachineRemoveFailureEvent(event);
  if (Option.isNone(decoded)) return;
  const attemptId = decoded.value.data.event.data.attemptId;
  const inngestRunId = decoded.value.data.run_id;
  await runEffect(
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
  step: StepTools<"run" | "sendEvent">;
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

export const createProcessMachineRemove = (
  inngest: PloyzInngest,
  runEffect: EffectRunner = runInngestEffect,
  observe: typeof observeOrganization = observeOrganization,
) =>
  inngest.createFunction(
  {
    id: PROCESS_MACHINE_REMOVE_FUNCTION_ID,
    retries: MACHINE_REMOVE_RETRIES,
    triggers: [{ event: machineRemoveRequested.eventType }],
    concurrency: [{ key: "event.data.attemptId", limit: 1 }],
    onFailure: async ({ event }) =>
      executeProcessMachineRemoveOnFailure({ event }, runEffect),
  },
  async ({ event, step, runId, attempt }) =>
    executeProcessMachineRemove({
      event,
      step,
      runId,
      attempt,
    }, runEffect, observe),
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
