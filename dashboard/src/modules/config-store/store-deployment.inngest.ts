import {
  cancelStoreDeploymentRun, forgetStoreDeploymentRun, recordStoreDeploymentRun, runStoreDeployment, type StoreEffectRunner,
  stopStoreDeploymentRun, storeDeploymentRunner, unclaimedStoreDeployments,
} from "#/modules/config-store/store-deployment.server";
import type { PloyzInngest } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  configDeploymentAdmittedEventType, createConfigDeploymentStartedEvent, githubBuildRunCompletedEvent, inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
} from "#/modules/inngest/events";
import {
  checkStoreGithubBuild, GITHUB_CHECK_INTERVAL, planStoreGithubBuilds, START_WITHIN_MINUTES,
  startStoreGithubBuild, storeGithubBuildOpen, type StoreGithubTarget,
} from "#/modules/config-store/store-github-builds.server";
import { runInngestEffect } from "#/server/run.server";

export const RUN_STORE_DEPLOYMENT_FUNCTION_ID = "run-store-deployment";

/**
 * Cloud's worker runs each admitted Config Store Deployment: one step claims, prepares, confirms and records it in
 * Rust, so no secret, session or raw SDK evidence is ever a step's input or output. Deployments of one Environment
 * run one at a time; the Store supersedes the one still queued when a newer one is admitted. The run is recorded
 * first, so its cancellation finds the Deployment; a run that fails or is cancelled stops its GitHub builds and the
 * Store records that it stopped.
 */
export const createRunStoreDeployment = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: RUN_STORE_DEPLOYMENT_FUNCTION_ID,
      // A retried step claims again: before it prepared, it runs; after, the Store records the outcome unknown.
      retries: 2,
      triggers: [{ event: configDeploymentAdmittedEventType }],
      concurrency: [{ key: "event.data.environmentId", limit: 1 }],
      onFailure: async ({ event }) => {
        const { organizationId, deploymentId } = event.data.event.data;
        return runEffect(stopStoreDeploymentRun(organizationId, deploymentId, event.data.run_id));
      },
    },
    async ({ event, step, runId }) => {
      await step.run("record-run", () => runEffect(recordStoreDeploymentRun(runId, event.data)));
      const github = await step.run("plan-builds", () => runEffect(planStoreGithubBuilds(event.data)));
      await Promise.all(github.map((target) => walkGithub(event.data.organizationId, target, step, runEffect)));
      const ran = await step.run("run-deployment", () => runEffect(runStoreDeployment(event.data, storeDeploymentRunner(runId))));
      await step.run("forget-run", () => runEffect(forgetStoreDeploymentRun(runId)));
      return ran;
    },
  );

/**
 * Every minute: a queued Deployment whose hand-off to the worker was lost goes to it again, unkeyed like "Deploy now",
 * since its admission's own event is deduplicated. A duplicate run finds it claimed and runs nothing.
 */
export const createRedispatchStoreDeployments = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    { id: "redispatch-store-deployments", retries: 1, triggers: [{ cron: "* * * * *" }], concurrency: [{ limit: 1 }] },
    async ({ step }) => {
      const unclaimed = await step.run("unclaimed", () => runEffect(unclaimedStoreDeployments(new Date())));
      if (unclaimed.length > 0) await step.sendEvent("dispatch", unclaimed.map(createConfigDeploymentStartedEvent));
      return { redispatched: unclaimed.map((deployment) => deployment.deploymentId) };
    },
  );

/** Someone cancelled a run of the worker in Inngest: it stops like a failed one. */
export const createCancelStoreDeployment = (inngest: PloyzInngest, runEffect: StoreEffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "cancel-store-deployment",
      retries: 3,
      triggers: [{ event: inngestFunctionCancelledEventType }],
      concurrency: [{ key: "event.data.run_id", limit: 1 }],
    },
    async ({ event, step }) => {
      // Only the worker's runs are recorded, so any other function's cancellation finds nothing to stop.
      const cancelled = decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event);
      return step.run("stop-run", () => runEffect(cancelStoreDeploymentRun(cancelled.data.run_id)));
    },
  );

type Step = Parameters<Parameters<PloyzInngest["createFunction"]>[1]>[0]["step"];

/**
 * One build's go on GitHub, before the runner claims the Deployment: start it, then wait for its run while the runner
 * checks in and pushes. Each wait first reads whether a final report already ended it; each timeout also asks GitHub,
 * which catches a completion that landed between waits. With a Builder after GitHub, the first wait is the start limit.
 * Whatever GitHub doesn't build, the runner builds on the servers when the walk has them next, or fails with why.
 */
async function walkGithub(organizationId: string, target: StoreGithubTarget, step: Step, runEffect: StoreEffectRunner) {
  const key = target.service;
  const started = await step.run(`github-start-${key}`, () => runEffect(startStoreGithubBuild(organizationId, target)));
  if (started.kind !== "dispatched") return;
  for (let check = 0; ; check += 1) {
    if (!(await step.run(`github-open-${key}-${check}`, () => runEffect(storeGithubBuildOpen(target))))) return;
    const startLimit = check === 0 && target.hasNext;
    const ended = await step.waitForEvent(`github-wait-${key}-${check}`, {
      event: githubBuildRunCompletedEvent, if: `async.data.runId == ${started.runId}`,
      timeout: startLimit ? `${START_WITHIN_MINUTES}m` : GITHUB_CHECK_INTERVAL,
    });
    const seen = { ended: ended !== null, startLimit };
    const found = await step.run(`github-check-${key}-${check}`, () => runEffect(checkStoreGithubBuild(organizationId, target, seen)));
    if (found === "done") return;
  }
}
