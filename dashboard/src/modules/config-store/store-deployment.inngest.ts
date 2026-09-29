import type { Effect } from "effect";
import { abandonStoreDeployment, runStoreDeployment } from "#/modules/config-store/store-deployment.server";
import { cancelStoreGithubBuilds } from "#/modules/config-store/config-store.server";
import type { PloyzInngest } from "#/modules/inngest/client";
import { configDeploymentAdmittedEventType, githubBuildRunCompletedEvent } from "#/modules/inngest/events";
import {
  checkStoreGithubBuild, GITHUB_CHECK_INTERVAL, planStoreGithubBuilds, START_WITHIN_MINUTES,
  startStoreGithubBuild, storeGithubBuildOpen, type StoreGithubTarget,
} from "#/modules/config-store/store-github-builds.server";
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
      onFailure: async ({ event }) => {
        const { organizationId, deploymentId } = event.data.event.data;
        // A crash mid-walk must not leave a GitHub run building or its grant open.
        await runEffect(cancelStoreGithubBuilds(organizationId, deploymentId));
        return runEffect(abandonStoreDeployment(deploymentId, storeDeploymentRunner(event.data.run_id)));
      },
    },
    async ({ event, step, runId }) => {
      const github = await step.run("plan-builds", () => runEffect(planStoreGithubBuilds(event.data)));
      await Promise.all(github.map((target) => walkGithub(event.data.organizationId, target, step, runEffect)));
      return step.run("run-deployment", () => runEffect(runStoreDeployment(event.data, storeDeploymentRunner(runId))));
    },
  );

type Step = Parameters<Parameters<PloyzInngest["createFunction"]>[1]>[0]["step"];

/**
 * One build's go on GitHub, before the runner claims the Deployment: start it, then wait for its run while the runner
 * checks in and pushes. Each wait first reads whether a final report already ended it; each timeout also asks GitHub,
 * which catches a completion that landed between waits. With a Builder after GitHub, the first wait is the start limit.
 * Whatever GitHub doesn't build, the runner builds on the servers when the walk has them next, or fails with why.
 */
async function walkGithub(organizationId: string, target: StoreGithubTarget, step: Step, runEffect: StoreDeploymentEffectRunner) {
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
