import { githubInstallationReceived, githubInstallationRepositoriesReceived, githubRepositoriesSyncRequested } from "#/modules/github/github.events";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { requiredEventData } from "#/modules/inngest/define-event";
import type { GithubSyncEffectRunner } from "#/modules/github/inngest-sync/sync";
import { stopGithubInstallationBuilds } from "#/modules/config-store/store-github-builds.server";
import {
  processGithubInstallationEvent,
  processGithubInstallationRepositoriesEvent,
} from "#/modules/github/github-installation.server";
import type { StoreServices } from "#/modules/config-store/store-deployment.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

export type GithubInstallationWebhookStepTools = Pick<
  PloyzStepTools,
  "run" | "sendEvent"
>;

export async function executeProcessGithubInstallationReceived(
  {
    event,
    step,
  }: {
    event: { data: unknown };
    step: GithubInstallationWebhookStepTools;
  },
  runEffect: GithubSyncEffectRunner,
  runStore: EffectRunner<StoreServices> = runInngestEffect,
) {
  const payload = requiredEventData(githubInstallationReceived)(event);
  const result = await step.run("process-installation-event", () =>
    runEffect(processGithubInstallationEvent(payload)),
  );
  // After the suspension or the forgetting is recorded, so the woken walks find it.
  if (result.stopBuildsFor.length > 0) {
    await step.run("stop-builds", () =>
      runStore(stopGithubInstallationBuilds(result.installationId, result.stopBuildsFor)),
    );
  }

  if (result.shouldSyncRepositories) {
    await step.sendEvent(
      "request-repository-sync",
      githubRepositoriesSyncRequested.create({
        installationId: result.installationId,
        reason: `installation.${payload.action}`,
      }),
    );
  }

  return result;
}

export async function executeProcessGithubInstallationRepositoriesReceived(
  {
    event,
    step,
  }: {
    event: { data: unknown };
    step: GithubInstallationWebhookStepTools;
  },
  runEffect: GithubSyncEffectRunner,
) {
  const payload = requiredEventData(githubInstallationRepositoriesReceived)(event);
  const result = await step.run(
    "process-installation-repositories-event",
    () => runEffect(processGithubInstallationRepositoriesEvent(payload)),
  );

  if (result.shouldSyncRepositories) {
    await step.sendEvent(
      "request-repository-sync",
      githubRepositoriesSyncRequested.create({
        installationId: result.installationId,
        reason: `installation_repositories.${payload.action}`,
      }),
    );
  }

  return result;
}

export const createProcessGithubInstallationReceived = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "process-github-installation-received",
    retries: 3,
    triggers: [{ event: githubInstallationReceived.eventType }],
    concurrency: [{ key: "event.data.installation.id", limit: 1 }],
  },
  async ({ event, step }) =>
    executeProcessGithubInstallationReceived(
      { event, step },
      runInngestEffect,
    ),
  );

export const createProcessGithubInstallationRepositoriesReceived = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: "process-github-installation-repositories-received",
      retries: 3,
      triggers: [{ event: githubInstallationRepositoriesReceived.eventType }],
      concurrency: [{ key: "event.data.installation.id", limit: 1 }],
    },
    async ({ event, step }) =>
      executeProcessGithubInstallationRepositoriesReceived(
        { event, step },
        runInngestEffect,
      ),
  );
