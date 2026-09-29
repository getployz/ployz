import type { Effect } from "effect";
import {
  observeStoreBranch,
  observeStoreCheckSuite,
  type StoreGithubServices,
} from "#/modules/config-store/store-github.server";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import {
  createConfigDeploymentAdmittedEvent,
  githubCheckSuiteReceivedEventType,
  githubPushReceivedEventType,
  type ConfigDeploymentAdmittedEventData,
} from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";

type StoreGithubEffectRunner = <A, E extends Error>(program: Effect.Effect<A, E, StoreGithubServices>) => Promise<A>;

/** Its own step after the Store's: a failed send retries only the send, since the observation is already applied. */
async function dispatch(step: Pick<PloyzStepTools, "sendEvent">, deployments: ConfigDeploymentAdmittedEventData[]) {
  if (deployments.length > 0) await step.sendEvent("dispatch", deployments.map(createConfigDeploymentAdmittedEvent));
  return { admitted: deployments.map((deployment) => deployment.deploymentId) };
}

/** A push reaches the Config Store: one branch at a time, so heads apply in the order Cloud reads them. */
export const createStoreGithubPush = (inngest: PloyzInngest, runEffect: StoreGithubEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-github-push",
      retries: 5,
      triggers: [{ event: githubPushReceivedEventType }],
      concurrency: [{ key: "event.data.branchKey", limit: 1 }],
    },
    async ({ event, step }) =>
      dispatch(step, await step.run("observe-branch", () => runEffect(observeStoreBranch(event.data)))),
  );

/** A check suite's result reaches the Config Store, which may let a deploy waiting for CI go. */
export const createStoreGithubCheckSuite = (inngest: PloyzInngest, runEffect: StoreGithubEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "store-github-check-suite",
      retries: 5,
      triggers: [{ event: githubCheckSuiteReceivedEventType }],
      concurrency: [{ key: "event.data.checkSuiteKey", limit: 1 }],
    },
    async ({ event, step }) =>
      dispatch(step, await step.run("observe-check-suite", () => runEffect(observeStoreCheckSuite(event.data)))),
  );
