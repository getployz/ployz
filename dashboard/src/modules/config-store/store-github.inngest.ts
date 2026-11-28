import { observeStoreBranch, observeStoreCheckSuite } from "#/modules/config-store/store-github.server";
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
import { githubCheckSuiteReceived, githubPullRequestReceived, githubPushReceived } from "#/modules/github/github.events";
import { configDeploymentAdmitted, configPrCheckRequested, configSweepRequested } from "#/modules/config-store/config-store.events";
import { recordReceiving, type StoreServices } from "#/modules/config-store/store-deployment.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

/**
 * Each its own step after the Store's, so a failed one retries alone: the observation is already applied. Runs to
 * dispatch go to the worker; Branches the Store is closing come off the Servers.
 */
export async function followUp(step: Pick<PloyzStepTools, "run" | "sendEvent">, done: Pick<StoreOutcome, "deployments" | "closing">, runEffect: EffectRunner<StoreServices>) {
  if (done.deployments.length > 0) await step.sendEvent("dispatch", done.deployments.map(configDeploymentAdmitted.create));
  const removals = done.closing.length > 0 ? await step.run("close", () => runEffect(closeStoreEnvironments(done.closing))) : [];
  return { admitted: done.deployments.map((deployment) => deployment.deploymentId), removals };
}

/** A push reaches the Config Store: one branch at a time, so heads apply in the order Cloud reads them. */
export const createStoreGithubPush = (inngest: PloyzInngest, runEffect: EffectRunner<StoreServices> = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-github-push",
      retries: 5,
      triggers: [{ event: githubPushReceived.eventType }],
      concurrency: [{ key: "event.data.branchKey", limit: 1 }],
    },
    async ({ event, step }) => {
      const followed = await followUp(step, {
        deployments: await step.run("observe-branch", () => runEffect(observeStoreBranch(event.data))), closing: [],
      }, runEffect);
      const receivedAt = event.ts;
      if (followed.admitted.length > 0 && receivedAt !== undefined) {
        await step.run("record-receiving", () => runEffect(recordReceiving(followed.admitted, receivedAt)));
      }
      return followed;
    },
  );

/** A check suite's result reaches the Config Store, which may let a deploy waiting for CI go. */
export const createStoreGithubCheckSuite = (inngest: PloyzInngest, runEffect: EffectRunner<StoreServices> = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-github-check-suite",
      retries: 5,
      triggers: [{ event: githubCheckSuiteReceived.eventType }],
      concurrency: [{ key: "event.data.checkSuiteKey", limit: 1 }],
    },
    async ({ event, step }) => followUp(step, {
      deployments: await step.run("observe-check-suite", () => runEffect(observeStoreCheckSuite(event.data))), closing: [],
    }, runEffect),
  );

/**
 * A pull request reaches the Config Store, which makes, retracks or closes its PR Environments; then Cloud publishes
 * its comment and check. One pull request at a time, so facts apply in the order Cloud reads them.
 */
export const createStorePullRequest = (inngest: PloyzInngest, runEffect: EffectRunner<StoreServices> = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-github-pull-request",
      retries: 5,
      triggers: [{ event: githubPullRequestReceived.eventType }],
      concurrency: [{ key: "event.data.pullRequestKey", limit: 1 }],
    },
    async ({ event, step }) => {
      const done = await step.run("observe-pull-request", () => runEffect(observeStorePullRequest(event.data)));
      const followed = await followUp(step, done, runEffect);
      const published = await step.run("publish-check", () => runEffect(publishStorePrCheck(event.data)));
      return { ...followed, ...published };
    },
  );

/**
 * A Store write or a Deployment may have moved a pull request's check or preview: publish them again, one post per pull request at
 * a time, once its writes settle for a few seconds.
 */
export const createStorePrCheck = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "store-pr-check",
      retries: 3,
      triggers: [{ event: configPrCheckRequested.eventType }],
      concurrency: [{ key: "event.data.pullRequestKey", limit: 1 }],
      debounce: { key: "event.data.pullRequestKey", period: "10s" },
    },
    async ({ event, step }) => step.run("publish-check", () => runInngestEffect(publishRequestedStorePrCheck(event.data))),
  );

/** Hourly: every Store closes idle Branches and deletes closing ones that left the Servers. */
export const createStoreSweep = (inngest: PloyzInngest) =>
  inngest.createFunction(
    { id: "store-sweep", retries: 3, triggers: [{ cron: "30 * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => followUp(step, await step.run("sweep", () => runInngestEffect(sweepStores(new Date()))), runInngestEffect),
  );

/** A write closed a Branch at once: its Organization's Store sweeps now, and Cloud does what the sweep left it. */
export const createStoreSweepRequested = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "store-sweep-requested", retries: 3, triggers: [{ event: configSweepRequested.eventType }],
      concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    },
    async ({ event, step }) =>
      followUp(step, await step.run("sweep", () => runInngestEffect(sweepStore(event.data.organizationId, new Date()))), runInngestEffect),
  );
