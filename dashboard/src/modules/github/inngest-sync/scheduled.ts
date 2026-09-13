import {
  createGithubRepositoriesSyncRequestedEvent,
} from "#/modules/inngest/events";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import type { GithubSyncEffectRunner } from "#/modules/github/inngest-sync/sync";
import { listAllGithubInstallationIds } from "#/modules/github/github.repository";
import { runInngestEffect } from "#/server/run.server";

export type GithubRepositoryScheduleStepTools = Pick<
  PloyzStepTools,
  "run" | "sendEvent"
>;

export async function executeScheduleGithubRepositorySync(
  { step }: { step: GithubRepositoryScheduleStepTools },
  runEffect: GithubSyncEffectRunner,
) {
  const installationIds = await step.run("list-installation-ids", () =>
    runEffect(listAllGithubInstallationIds()),
  );
  await step.sendEvent(
    "request-repository-syncs",
    installationIds.map((installationId) =>
      createGithubRepositoriesSyncRequestedEvent({
        installationId,
        reason: "scheduled",
      }),
    ),
  );
  return { installationCount: installationIds.length };
}

export const createScheduleGithubRepositorySync = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "schedule-github-repository-sync",
    retries: 3,
    triggers: [{ cron: "TZ=UTC 0 2 * * *" }],
    concurrency: [{ limit: 1 }],
    },
    async ({ step }) =>
      executeScheduleGithubRepositorySync(
        { step },
        runInngestEffect,
      ),
  );
