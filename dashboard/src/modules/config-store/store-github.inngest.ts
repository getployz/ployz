import { observeStoreBranch, observeStoreCheckSuite } from "#/modules/config-store/store-github.server";
import type { StoreEffectRunner } from "#/modules/config-store/store-deployment.server";
import {
  closeStoreEnvironments,
  observeStorePullRequest,
  publishRequestedStorePrCheck,
  publishStorePrCheck,
  sweepStore,
  sweepStores,
  type StoreOutcome,
} from "#/modules/config-store/store-pull-request.server";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import {
  configPrCheckRequestedEventType,
  configSweepRequestedEventType,
  createConfigDeploymentAdmittedEvent,
  githubCheckSuiteReceivedEventType,
  githubPullRequestReceivedEventType,
  githubPushReceivedEventType,
} from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";

/**
 * Each its own step after the Store's, so a failed one retries alone: the observation is already applied. Runs to
 * dispatch go to the worker; Branches the Store is closing come off the Servers.
 */
export async function followUp(step: Pick<PloyzStepTools, "run" | "sendEvent">, done: Pick<StoreOutcome, "deployments" | "closing">, runEffect: StoreEffectRunner) {
  if (done.deployments.length > 0) await step.sendEvent("dispatch", done.deployments.map(createConfigDeploymentAdmittedEvent));
  const removals = done.closing.length > 0 ? await step.run("close", () => runEffect(closeStoreEnvironments(done.closing))) : [];
  return { admitted: done.deployments.map((deployment) => deployment.deploymentId), removals };
}

/** A push reaches the Config Store: one branch at a time, so heads apply in the order Cloud reads them. */
export const createStoreGithubPush = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-github-push",
      retries: 5,
      triggers: [{ event: githubPushReceivedEventType }],
      concurrency: [{ key: "event.data.branchKey", limit: 1 }],
    },
    async ({ event, step }) => followUp(step, {
      deployments: await step.run("observe-branch", () => runEffect(observeStoreBranch(event.data))), closing: [],
    }, runEffect),
  );

/** A check suite's result reaches the Config Store, which may let a deploy waiting for CI go. */
export const createStoreGithubCheckSuite = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-github-check-suite",
      retries: 5,
      triggers: [{ event: githubCheckSuiteReceivedEventType }],
      concurrency: [{ key: "event.data.checkSuiteKey", limit: 1 }],
    },
    async ({ event, step }) => followUp(step, {
      deployments: await step.run("observe-check-suite", () => runEffect(observeStoreCheckSuite(event.data))), closing: [],
    }, runEffect),
  );

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

/**
 * A Store write or a Deployment may have moved a pull request's check: publish it again, one post per pull request at
 * a time, once its writes settle for a few seconds.
 */
export const createStorePrCheck = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-pr-check",
      retries: 3,
      triggers: [{ event: configPrCheckRequestedEventType }],
      concurrency: [{ key: "event.data.pullRequestKey", limit: 1 }],
      debounce: { key: "event.data.pullRequestKey", period: "10s" },
    },
    async ({ event, step }) => ({
      check: await step.run("publish-check", () => runEffect(publishRequestedStorePrCheck(event.data))),
    }),
  );

/** Hourly: every Store closes idle Branches and deletes closing ones that left the Servers. */
export const createStoreSweep = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    { id: "store-sweep", retries: 3, triggers: [{ cron: "30 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => followUp(step, await step.run("sweep", () => runEffect(sweepStores(new Date()))), runEffect),
  );

/** A write closed a Branch at once: its Organization's Store sweeps now, and Cloud does what the sweep left it. */
export const createStoreSweepRequested = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-sweep-requested", retries: 3, triggers: [{ event: configSweepRequestedEventType }],
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    },
    async ({ event, step }) =>
      followUp(step, await step.run("sweep", () => runEffect(sweepStore(event.data.organizationId, new Date()))), runEffect),
  );
