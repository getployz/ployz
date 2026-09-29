import type { Effect } from "effect";
import { abandonStoreDeployment, runStoreDeployment } from "#/modules/config-store/store-deployment.server";
import type { PloyzInngest } from "#/modules/inngest/client";
import { configDeploymentAdmittedEventType } from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";
import type { StoreDeploymentServices } from "#/modules/config-store/store-deployment.server";

type StoreDeploymentEffectRunner = <A, E extends Error>(
  program: Effect.Effect<A, E, StoreDeploymentServices>,
) => Promise<A>;

export const RUN_STORE_DEPLOYMENT_FUNCTION_ID = "run-store-deployment";

/** A run is one durable runner: its retried steps claim as the same runner, and a duplicate delivery is another. */
export const storeDeploymentRunner = (runId: string) => `cloud-${runId}`;

/**
 * Cloud's worker runs each admitted Config Store Deployment: one step claims, prepares, confirms and records it in
 * Rust, so no secret, session or raw SDK evidence is ever a step's input or output. Deployments of one Environment
 * run one at a time; the Store supersedes the one still queued when a newer one is admitted.
 */
export const createRunStoreDeployment = (inngest: PloyzInngest, runEffect: StoreDeploymentEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: RUN_STORE_DEPLOYMENT_FUNCTION_ID,
      // A retried step claims again: before it prepared, it runs; after, the Store records the outcome unknown.
      retries: 2,
      triggers: [{ event: configDeploymentAdmittedEventType }],
      concurrency: [{ key: "event.data.environmentId", limit: 1 }],
      onFailure: ({ event }) =>
        runEffect(abandonStoreDeployment(
          event.data.event.data.organizationId, event.data.event.data.deploymentId, storeDeploymentRunner(event.data.run_id),
        )),
    },
    async ({ event, step, runId }) =>
      step.run("run-deployment", () => runEffect(runStoreDeployment(event.data, storeDeploymentRunner(runId)))),
  );
