import { Effect, Option, Schema } from "effect";
import {
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
  prepareMachineRemoveAttemptActivity,
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

type StepTools = Pick<PloyzStepTools, "run">;

export async function executeProcessMachineRemove({
  event,
  step,
  runId,
}: {
  event: { data: unknown };
  step: StepTools;
  runId: string;
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
  const removed = await step.run("remove-machine", () =>
    runInngestEffect(Effect.scoped(removeMachineActivity(attempt))),
  );

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

  await step.run("complete-succeeded", () =>
    runInngestEffect(
      completeMachineRemoveAttemptActivity({
        attemptId,
        inngestRunId: runId,
        completion: { state: "succeeded" },
        now: new Date(),
      }),
    ),
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
}: {
  event: unknown;
  step: StepTools;
}) {
  const decoded = await step.run("decode-machine-remove-cancellation-event", () =>
    decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event),
  );
  if (decoded.data.function_id !== PROCESS_MACHINE_REMOVE_FUNCTION_ID) {
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
    retries: 5,
    triggers: [{ event: machineRemoveRequestedEventType }],
    concurrency: [{ key: "event.data.attemptId", limit: 1 }],
    onFailure: async ({ event }) =>
      executeProcessMachineRemoveOnFailure({ event }),
  },
  async ({ event, step, runId }) =>
    executeProcessMachineRemove({
      event,
      step,
      runId,
    }),
  );

export const createCancelMachineRemove = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "cancel-machine-remove",
    retries: 3,
    triggers: [{ event: inngestFunctionCancelledEventType }],
    concurrency: [{ key: "event.data.run_id", limit: 1 }],
  },
  async ({ event, step }) =>
    executeCancelMachineRemove({ event, step }),
  );
