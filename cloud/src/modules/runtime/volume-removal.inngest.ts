import { Effect, Option, Schema } from "effect";
import {
  inngestEventEnvelopeFields,
  inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
  inngestFunctionFailedEnvelopeSchema,
  volumeRemoveRequestedEventType,
} from "#/modules/inngest/events";
import type {
  PloyzInngest,
  PloyzStepTools,
} from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { PROCESS_VOLUME_REMOVE_FUNCTION_ID } from "#/modules/inngest/row-backed-workflow-ids";
import {
  cancelVolumeRemoveAttemptActivity,
  completeVolumeRemoveAttemptActivity,
  failOwnedVolumeRemoveAttemptActivity,
  prepareVolumeRemoveAttemptActivity,
  reconcileVolumeRemoveTombstoneActivity,
  removeVolumesActivity,
  volumeRemoveCompletion,
} from "#/modules/runtime/volume-removal.server";
import { runInngestEffect } from "#/server/run.server";
import { reviveDurableAttemptDates } from "#/modules/runtime/durable-attempt-dates";

export { PROCESS_VOLUME_REMOVE_FUNCTION_ID };

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const VolumeRemoveRequestedData = Schema.Struct({ attemptId: NonEmptyString });
const VolumeRemoveRequestedEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal("cloud/volume-remove.requested"),
  data: VolumeRemoveRequestedData,
});
const VolumeRemoveFailureEnvelope = inngestFunctionFailedEnvelopeSchema(
  VolumeRemoveRequestedEnvelope,
);

export const decodeVolumeRemoveFailureEvent = Schema.decodeUnknownOption(
  VolumeRemoveFailureEnvelope,
);

type StepTools = Pick<PloyzStepTools, "run">;

export async function executeProcessVolumeRemove({
  event,
  step,
  runId,
}: {
  event: { data: unknown };
  step: StepTools;
  runId: string;
}) {
  const attemptId = await step.run(
    "normalize-volume-remove-attempt-id",
    () => {
      const decoded = Schema.decodeUnknownOption(VolumeRemoveRequestedData)(
        event.data,
        { onExcessProperty: "preserve" },
      );
      return Option.isSome(decoded) ? decoded.value.attemptId : null;
    },
  );
  if (attemptId === null) {
    return { attemptId: null, status: "invalid", skipped: true };
  }

  const serialized = await step.run("claim-volume-remove", () =>
    runInngestEffect(
      prepareVolumeRemoveAttemptActivity({
        attemptId,
        inngestRunId: runId,
        now: new Date(),
      }),
    ),
  );
  const prepared = serialized.kind === "missing"
    ? serialized
    : {
        ...serialized,
        attempt: reviveDurableAttemptDates(serialized.attempt),
      };
  if (prepared.kind === "missing") {
    return { attemptId, status: "missing", skipped: true };
  }
  if (prepared.kind === "terminal") {
    return {
      attemptId: prepared.attempt.id,
      status: prepared.attempt.status,
      skipped: true,
    };
  }
  if (prepared.kind === "reconcile") {
    await step.run("reconcile-volume-remove-tombstone", () =>
      runInngestEffect(
        reconcileVolumeRemoveTombstoneActivity(prepared.attempt),
      ),
    );
    return {
      attemptId: prepared.attempt.id,
      status: prepared.attempt.status,
      skipped: true,
    };
  }

  const attempt = prepared.attempt;
  const outcome = await step.run("remove-volumes", () =>
    runInngestEffect(Effect.scoped(removeVolumesActivity(attempt))),
  );
  const completion = volumeRemoveCompletion(attempt, outcome);
  const completed = await step.run("persist-volume-remove-outcome", () =>
    runInngestEffect(
      completeVolumeRemoveAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: runId,
        ...completion,
        now: new Date(),
      }),
    ),
  );
  if (completion.status === "completed") {
    await step.run("reconcile-volume-remove-tombstone", () =>
      runInngestEffect(reconcileVolumeRemoveTombstoneActivity(completed)),
    );
  }
  return { attemptId, status: completion.status };
}

export async function executeProcessVolumeRemoveOnFailure({
  event,
}: {
  event: unknown;
}) {
  const decoded = decodeVolumeRemoveFailureEvent(event);
  if (Option.isNone(decoded)) return;
  await runInngestEffect(
    failOwnedVolumeRemoveAttemptActivity({
      attemptId: decoded.value.data.event.data.attemptId,
      inngestRunId: decoded.value.data.run_id,
      failureMessage:
        decoded.value.data.error.message || "Volume remove retries were exhausted.",
      now: new Date(),
    }),
  );
}

export async function executeCancelVolumeRemove({
  event,
  step,
}: {
  event: unknown;
  step: StepTools;
}) {
  const decoded = await step.run("decode-volume-remove-cancellation-event", () =>
    decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event),
  );
  if (decoded.data.function_id !== PROCESS_VOLUME_REMOVE_FUNCTION_ID) {
    return { skipped: true };
  }
  return step.run("cancel-volume-remove", () =>
    runInngestEffect(
      cancelVolumeRemoveAttemptActivity({
        inngestRunId: decoded.data.run_id,
        now: new Date(),
      }),
    ),
  );
}

export const createProcessVolumeRemove = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: PROCESS_VOLUME_REMOVE_FUNCTION_ID,
    retries: 5,
    triggers: [{ event: volumeRemoveRequestedEventType }],
    concurrency: [{ key: "event.data.attemptId", limit: 1 }],
    onFailure: async ({ event }) =>
      executeProcessVolumeRemoveOnFailure({ event }),
  },
  async ({ event, step, runId }) =>
    executeProcessVolumeRemove({
      event,
      step,
      runId,
    }),
  );

export const createCancelVolumeRemove = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "cancel-volume-remove",
    retries: 3,
    triggers: [{ event: inngestFunctionCancelledEventType }],
    concurrency: [{ key: "event.data.run_id", limit: 1 }],
  },
  async ({ event, step }) =>
    executeCancelVolumeRemove({ event, step }),
  );
