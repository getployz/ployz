import { Effect } from "effect";
import { configDeploymentStopRequested } from "#/modules/config-store/config-store.events";
import { type StoreServices, stopStoreDeploymentRun } from "#/modules/config-store/store-deployment.server";
import type { PloyzInngest } from "#/modules/inngest/client";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

type Step = Parameters<Parameters<PloyzInngest["createFunction"]>[1]>[0]["step"];

type StopRequest = typeof configDeploymentStopRequested.schema.Type;
/** Tries of a stop in one round, before it hands itself to the next. */
export const STOP_TRIES = 40;
/** The longest wait between tries of a stop. */
const STOP_WAIT_CAP_SECONDS = 600;

/**
 * Stop the run, once. A stop the Store didn't answer cancelled nothing and is handed to `stop-store-deployment`, which
 * tries until it does: a run's record may still land, so Cloud never gives the stop up.
 */
export const stopRun = async (step: Step, runEffect: EffectRunner<StoreServices>, request: StopRequest) => {
  const stopped = await step.run("stop-run", () => runEffect(stopOnce(request)));
  if ("unsettled" in stopped) await step.sendEvent("stop-later", configDeploymentStopRequested.create(request));
  return stopped;
};

/** One try of a stop: its answer, or `unsettled` when the Store didn't give one. */
const stopOnce = ({ organizationId, deploymentId, runId }: StopRequest) => stopStoreDeploymentRun(organizationId, deploymentId, runId).pipe(
  Effect.catchCause((cause) => Effect.logWarning("A Deployment's stop got no answer from the Store; trying again.", cause).pipe(
    Effect.as({ unsettled: deploymentId }),
  )),
);

/**
 * A stop the Store didn't answer: try it again, waiting longer each time up to ten minutes, and after a round of
 * tries hand it to the next round. It ends only once the Store answered it. A hand-off that never lands leaves the
 * run recorded, and the sweep (`staleStoreDeploymentRuns`) hands it on again.
 */
export const createStopStoreDeployment = (inngest: PloyzInngest, runEffect: EffectRunner<StoreServices> = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "stop-store-deployment",
      // A failed hand-off to the next round runs again; each try's answer is kept.
      retries: 3,
      triggers: [{ event: configDeploymentStopRequested.eventType }],
      // One stop at a time per Deployment: an older run's stop never races a newer one's.
      concurrency: [{ key: "event.data.deploymentId", limit: 1 }],
    },
    async ({ event, step }) => {
      const request = event.data;
      for (let attempt = 0; attempt < STOP_TRIES; attempt += 1) {
        await step.sleep(`wait-${attempt}`, `${Math.min(2 ** attempt, STOP_WAIT_CAP_SECONDS)}s`);
        const stopped = await step.run(`stop-${attempt}`, () => runEffect(stopOnce(request)));
        if (!("unsettled" in stopped)) return stopped;
      }
      await step.sendEvent("next-round", configDeploymentStopRequested.create({ ...request, round: request.round + 1 }));
      return { round: request.round + 1 };
    },
  );
