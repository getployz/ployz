import { Option, Schema } from "effect";
import {
  closeStaleCleanups, type CleanupEnd, endCleanup, recordCleanupWaiting, retireOldCopies,
} from "#/modules/config-store/retire-old.server";
import { attemptLifecycle, type RunEnd } from "#/modules/inngest/attempt-lifecycle";
import { inngestRunStatus, type PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { configDeploymentAdmittedEventType, configDeploymentRetireRequestedEventType } from "#/modules/inngest/events";
import type { runInngestEffect } from "#/server/run.server";

export const RETIRE_ATTEMPTS = 3;
export const RETIRE_PAUSE = "1m";

const retireRequestedSchema = Schema.Struct({
  data: Schema.Struct({ organizationId: Schema.String, environmentId: Schema.String, deploymentId: Schema.String }),
});

/** After a Cloud Deploy ran, retire the old copies its run replaced, through the Store. */
async function retireOld(
  { event, step, runId }: { event: { data: unknown }; step: PloyzStepTools; runId: string },
  runEffect: typeof runInngestEffect,
) {
  const { data } = decodeInngestEnvelope(retireRequestedSchema)(event);
  await step.run("record-waiting", () => runEffect(recordCleanupWaiting(runId, data)));
  for (let attempt = 1; ; attempt += 1) {
    const { left } = await step.run(`retire-${attempt}`, () => runEffect(retireOldCopies(data)));
    const end: CleanupEnd | null = left.length === 0 ? { state: "done" }
      : attempt === RETIRE_ATTEMPTS ? { state: "left_running", left }
      : null;
    if (end !== null) {
      await step.run("end", () => runEffect(endCleanup(runId, end, Option.some(data))));
      return end;
    }
    await step.run(`waiting-${attempt}`, () => runEffect(recordCleanupWaiting(runId, data, left)));
    await step.sleep(`pause-${attempt}`, RETIRE_PAUSE);
  }
}

const runEnds = {
  failure: (error) => ({ state: "failed", message: error?.message }),
  cancellation: () => ({ state: "cancelled" }),
} satisfies Record<RunEnd, (error?: Error) => CleanupEnd>;

const lifecycle = attemptLifecycle({
  run: {
    id: "retire-old",
    triggers: [{ event: configDeploymentRetireRequestedEventType }],
    concurrency: [{ key: "event.data.environmentId", limit: 1 }],
    cancelOn: [{
      event: configDeploymentAdmittedEventType,
      if: "async.data.environmentId == event.data.environmentId && async.data.deploymentId != event.data.deploymentId",
    }],
    handler: retireOld,
  },
  cancelId: "cancel-retire-old",
  triggeringEvent: retireRequestedSchema,
  triggeringOnFailure: true,
  closeRun: (runId, end, triggering, error) =>
    endCleanup(runId, runEnds[end](error), Option.map(triggering, ({ data }) => data)),
  sweep: { id: "close-stale-deployment-cleanups", closeStale: () => closeStaleCleanups(inngestRunStatus) },
});

export const createRetireOld = lifecycle.createRun;
export const createCancelRetireOld = lifecycle.createCancel;
export const createRetireOldFunctions = lifecycle.createFunctions;
