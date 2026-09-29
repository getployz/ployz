import {
  closeStoreEnvironments,
  observeStorePullRequest,
  publishStorePrCheck,
  sweepStores,
  type StoreOutcome,
} from "#/modules/config-store/store-pull-request.server";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { createConfigDeploymentAdmittedEvent, githubPullRequestReceivedEventType } from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";

type StoreEffectRunner = typeof runInngestEffect;

/** Each its own step after the Store's, so a failed one retries alone: the observation is already applied. */
async function followUp(step: Pick<PloyzStepTools, "run" | "sendEvent">, done: StoreOutcome, runEffect: StoreEffectRunner) {
  if (done.deployments.length > 0) await step.sendEvent("dispatch", done.deployments.map(createConfigDeploymentAdmittedEvent));
  const closed = done.closing.length > 0 ? await step.run("close", () => runEffect(closeStoreEnvironments(done.closing))) : 0;
  return { admitted: done.deployments.map((deployment) => deployment.deploymentId), closing: closed };
}

/**
 * A pull request reaches the Config Store, which makes, retracks or closes its PR Environments; then Cloud publishes
 * its check. One pull request at a time, so facts apply in the order Cloud reads them.
 */
export const createStorePullRequest = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-github-pull-request",
      retries: 5,
      triggers: [{ event: githubPullRequestReceivedEventType }],
      concurrency: [{ key: "event.data.pullRequestKey", limit: 1 }],
    },
    async ({ event, step }) => {
      const done = await step.run("observe-pull-request", () => runEffect(observeStorePullRequest(event.data)));
      const followed = await followUp(step, done, runEffect);
      const check = done.check ? await step.run("publish-check", () => runEffect(publishStorePrCheck(event.data))) : "skipped";
      return { ...followed, check };
    },
  );

/** Hourly, every Store closes idle Branches and deletes closing ones that left the Servers. */
export const createStoreSweep = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    { id: "store-sweep", retries: 3, triggers: [{ cron: "30 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => followUp(step, await step.run("sweep", () => runEffect(sweepStores(new Date()))), runEffect),
  );
