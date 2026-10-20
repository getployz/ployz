import { endCleanup, type CleanupEnd, recordCleanupWaiting, retireOldCopies } from "#/modules/config-store/retire-old.server";
import type { StoreEffectRunner } from "#/modules/config-store/store-deployment.server";
import type { PloyzInngest } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  configDeploymentAdmittedEventType, configDeploymentRetireRequestedEventType, inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
} from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";

export const RETIRE_OLD_FUNCTION_ID = "retire-old";
export const RETIRE_ATTEMPTS = 3;
export const RETIRE_PAUSE = "1m";

/** After a Cloud Deploy ran, retire the old copies its run replaced, through the Store. */
export const createRetireOld = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: RETIRE_OLD_FUNCTION_ID,
      retries: 2,
      triggers: [{ event: configDeploymentRetireRequestedEventType }],
      concurrency: [{ key: "event.data.environmentId", limit: 1 }],
      cancelOn: [{
        event: configDeploymentAdmittedEventType, match: "data.environmentId", if: "async.data.deploymentId != event.data.deploymentId",
      }],
      onFailure: async ({ event, error }) => runEffect(endCleanup(event.data.run_id, { state: "failed", message: error.message })),
    },
    async ({ event, step, runId }) => {
      await step.run("record-waiting", () => runEffect(recordCleanupWaiting(runId, event.data)));
      for (let attempt = 1; ; attempt += 1) {
        const { left } = await step.run(`retire-${attempt}`, () => runEffect(retireOldCopies(event.data)));
        const end: CleanupEnd | null = left.length === 0 ? { state: "done" }
          : attempt === RETIRE_ATTEMPTS ? { state: "left_running", left }
          : null;
        if (end !== null) {
          await step.run("end", () => runEffect(endCleanup(runId, end)));
          return end;
        }
        await step.run(`waiting-${attempt}`, () => runEffect(recordCleanupWaiting(runId, event.data, left)));
        await step.sleep(`pause-${attempt}`, RETIRE_PAUSE);
      }
    },
  );

const isRetireOld = (functionId: string) =>
  functionId === RETIRE_OLD_FUNCTION_ID || functionId === `ployz-cloud-${RETIRE_OLD_FUNCTION_ID}`;

/** A newer Deployment took over a cleanup, or someone cancelled it in Inngest: its row ends cancelled. */
export const createCancelRetireOld = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "cancel-retire-old",
      retries: 3,
      triggers: [{
        event: inngestFunctionCancelledEventType,
        if: `event.data.function_id == '${RETIRE_OLD_FUNCTION_ID}' || event.data.function_id == 'ployz-cloud-${RETIRE_OLD_FUNCTION_ID}'`,
      }],
      concurrency: [{ key: "event.data.run_id", limit: 1 }],
    },
    async ({ event, step }) => {
      const cancelled = decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event);
      if (!isRetireOld(cancelled.data.function_id)) return { skipped: true };
      return { cancelled: await step.run("cancel-cleanup", () => runEffect(endCleanup(cancelled.data.run_id, { state: "cancelled" }))) };
    },
  );

export const createRetireOldFunctions = (inngest: PloyzInngest) => [createRetireOld(inngest), createCancelRetireOld(inngest)] as const;
