import { Effect, Schema } from "effect";
import { listInstallationReposPage } from "#/modules/github/github.api";
import type { GithubApi } from "#/modules/github/github-observation.api";
import {
  lockGithubRepositoryCache,
  markGithubRepositoriesSynced,
  pruneCachedGithubRepositories,
  upsertCachedGithubRepositories,
} from "#/modules/github/github.repository";
import { Database } from "#/server/database.server";
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

/**
 * Caches every repository GitHub grants the installation and drops the rest, in one transaction holding the
 * installation's cache lock, so its writes reflect one listing and no other writer interleaves with them.
 */
export const syncGithubRepositories = Effect.fn("Github.syncRepositories")(function* (installationId: number) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    yield* lockGithubRepositoryCache(installationId);
    const syncedAt = new Date();
    const listedRepositoryIds: number[] = [];
    let processedPageCount = 0;
    for (let pageNumber = 1; ; pageNumber++) {
      const page = yield* listInstallationReposPage(installationId, pageNumber);
      yield* upsertCachedGithubRepositories({ installationId, repositories: page.repositories, syncedAt });
      listedRepositoryIds.push(...page.repositories.map((repository) => repository.id));
      processedPageCount += 1;
      if (!page.hasNextPage) break;
    }
    const deletedRepositoryCount = yield* pruneCachedGithubRepositories(installationId, listedRepositoryIds);
    yield* markGithubRepositoriesSynced(installationId, syncedAt);
    return { processedPageCount, processedRepoCount: listedRepositoryIds.length, deletedRepositoryCount };
  }));
});

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
  const result = await step.run("sync-repositories", () =>
    runEffect(syncGithubRepositories(installationId)),
  );
  return { installationId, ...result };
}

export const createSyncGithubRepositories = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "sync-github-repositories",
    retries: 3,
    triggers: [{ event: githubRepositoriesSyncRequestedEventType }],
    singleton: SYNC_GITHUB_REPOSITORIES_SINGLETON,
    // The unkeyed limit spans installations, so syncs holding a connection while they list GitHub leave most of a pool of 10.
    concurrency: [{ key: "event.data.installationId", limit: 1 }, { limit: 4 }],
  },
  async ({ event, step }) =>
    executeSyncGithubRepositories(
      { event, step },
      runInngestEffect,
    ),
  );
