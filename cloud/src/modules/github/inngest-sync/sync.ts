import { Effect, Schema } from "effect";
import { listInstallationReposPage } from "#/modules/github/github.api";
import type { GithubApi } from "#/modules/github/github-observation.api";
import {
  listGithubInstallationUserIds,
  pruneCachedGithubRepositories,
  upsertCachedGithubRepositories,
} from "#/modules/github/github.repository";
import type { Database } from "#/server/database.server";
import type { AppConfig } from "#/server/config.server";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { githubRepositoriesSyncRequestedEventType } from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";

export const SYNC_GITHUB_REPOSITORIES_SINGLETON = {
  key: "event.data.installationId",
  mode: "skip",
} as const;

const GithubRepositorySyncEventData = Schema.Struct({
  installationId: Schema.Number.check(
    Schema.isInt(),
    Schema.isGreaterThan(0),
  ),
  reason: Schema.optionalKey(Schema.String),
});

export type GithubRepositorySyncStepTools = Pick<PloyzStepTools, "run">;

export type GithubSyncEffectRunner = <A, E extends Error>(
  effect: Effect.Effect<A, E, Database | AppConfig | GithubApi>,
) => Promise<A>;

export async function executeSyncGithubRepositories(
  {
    event,
    step,
  }: {
    event: { data: unknown };
    step: GithubRepositorySyncStepTools;
  },
  runEffect: GithubSyncEffectRunner,
) {
  const installationId = await step.run("decode-repository-sync-event", () =>
    decodeInngestEnvelope(GithubRepositorySyncEventData)(event.data, {
      onExcessProperty: "preserve",
    }).installationId,
  );
  const syncedAtIso = await step.run("start-sync", () =>
    new Date().toISOString(),
  );
  const syncedAt = new Date(syncedAtIso);
  let currentPage = 1;
  let processedPageCount = 0;
  let processedRepoCount = 0;
  const userIds = await step.run("list-installation-users", () =>
    runEffect(listGithubInstallationUserIds(installationId)),
  );

  while (true) {
    const pageNumber = currentPage;
    const repositoryPage = await step.run(`sync-page-${pageNumber}`, () =>
      runEffect(
        Effect.gen(function* () {
          const page = yield* listInstallationReposPage(
            installationId,
            pageNumber,
          );
          yield* upsertCachedGithubRepositories({
            userIds,
            installationId,
            repositories: page.repositories,
            syncedAt,
          });
          return page;
        }),
      ),
    );

    processedPageCount += 1;
    processedRepoCount += repositoryPage.repositories.length;
    if (!repositoryPage.hasNextPage) break;
    currentPage += 1;
  }

  const deletedRepositoryCount = await step.run(
    "prune-stale-repositories",
    () => runEffect(pruneCachedGithubRepositories(installationId, syncedAt)),
  );
  return {
    installationId,
    processedPageCount,
    processedRepoCount,
    deletedRepositoryCount,
  };
}

export const createSyncGithubRepositories = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "sync-github-repositories",
    retries: 3,
    triggers: [{ event: githubRepositoriesSyncRequestedEventType }],
    singleton: SYNC_GITHUB_REPOSITORIES_SINGLETON,
    concurrency: [{ key: "event.data.installationId", limit: 1 }],
  },
  async ({ event, step }) =>
    executeSyncGithubRepositories(
      { event, step },
      runInngestEffect,
    ),
  );
