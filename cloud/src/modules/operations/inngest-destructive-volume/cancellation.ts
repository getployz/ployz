import type { PloyzInngest } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
} from "#/modules/inngest/events";
import { finalizeUnassociatedDestructiveVolumeAttempt } from "#/modules/operations/destructive-volume-attempt.repository";
import {
  closeDestructiveVolumeWatchAndReconcile,
  loadDestructiveVolumeCancellationContext,
} from "#/modules/operations/destructive-volume-workflow.server";
import { runInngestEffect } from "#/server/run.server";
import { PROCESS_DESTRUCTIVE_VOLUME_FUNCTION_ID } from "./processor";

export const createCancelDestructiveVolume = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "cancel-destructive-volume",
      retries: 3,
      triggers: [{ event: inngestFunctionCancelledEventType }],
      concurrency: [{ key: "event.data.run_id", limit: 1 }],
    },
    async ({ event, step }) => {
      const decodedEvent = await step.run(
        "decode-destructive-volume-cancellation-event",
        () => decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event),
      );
      const functionId = decodedEvent.data.function_id;
      const runId = decodedEvent.data.run_id;
      if (functionId !== PROCESS_DESTRUCTIVE_VOLUME_FUNCTION_ID) {
        return { skipped: true };
      }
      const context = await step.run(
        "load-destructive-volume-cancellation-context",
        () =>
          runInngestEffect(
            loadDestructiveVolumeCancellationContext({ inngestRunId: runId }),
          ),
      );
      if (!context || context.attempt.state === "terminal") return { skipped: true };
      if (
        (context.attempt.state === "owned_unassociated" ||
          context.attempt.state === "owned_associated") &&
        context.attempt.inngestRunId !== runId
      ) {
        throw new Error("Destructive volume workflow has another durable owner.");
      }
      if (context.attempt.state === "owned_associated") {
        const attempt = context.attempt;
        return step.run("close-cancelled-destructive-volume-watch", () =>
          runInngestEffect(
            closeDestructiveVolumeWatchAndReconcile({
              attemptId: attempt.id,
              organizationId: context.organizationId,
              operationId: attempt.operationId,
              expectedInngestRunId: runId,
              state: "cloud_cancelled",
            }),
          ),
        );
      }
      return step.run("cancel-unassociated-destructive-volume", () =>
        runInngestEffect(
          finalizeUnassociatedDestructiveVolumeAttempt({
            attemptId: context.attempt.id,
            organizationId: context.organizationId,
            expectedInngestRunId: runId,
            event: "cloud_cancelled",
            message:
              "Destructive volume workflow was cancelled before Core association.",
          }),
        ),
      );
    },
  );
