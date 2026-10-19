import { sql } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { listInstallationReposPage } from "#/modules/github/github.api";
import type { GithubInstallationReposPage } from "#/modules/github/github";
import type { GithubApi } from "#/modules/github/github-observation.api";
import {
  lockGithubRepositoryCache,
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

/** Every page GitHub lists for the installation. Given an earlier listing, re-reads each page conditionally against it. */
const listInstallationRepos = Effect.fn("Github.listInstallationRepos")(
  function* (installationId: number, previous: readonly GithubInstallationReposPage[] = []) {
    const pages: GithubInstallationReposPage[] = [];
    for (let pageNumber = 1; ; pageNumber++) {
      const page = yield* listInstallationReposPage(installationId, pageNumber, previous[pageNumber - 1]);
      pages.push(page);
      if (!page.hasNextPage) return pages;
    }
  },
);

/**
 * Caches every repository GitHub grants the installation and drops the rest. It lists GitHub before it holds a
 * connection, then lists again inside one transaction holding the installation's cache lock, conditionally, so pages
 * GitHub hasn't changed cost a 304, and writes that second listing. With `lockDeadline`, a Date.now() time, gives up
 * with a LockTimeoutError when another writer still holds the lock then.
 */
export const syncGithubRepositories = Effect.fn("Github.syncRepositories")(
  function* (installationId: number, lockDeadline?: number) {
    const listed = yield* listInstallationRepos(installationId);
    const database = yield* Database;
    return yield* database.transaction(Effect.gen(function* () {
      if (lockDeadline !== undefined) {
        const lockWaitMillis = Math.max(1, lockDeadline - Date.now());
        yield* database.drizzle.execute(sql`select set_config('lock_timeout', ${`${lockWaitMillis}ms`}, true)`);
      }
      yield* lockGithubRepositoryCache(installationId);
      const pages = yield* listInstallationRepos(installationId, listed);
      const syncedAt = new Date();
      for (const page of pages) {
        yield* upsertCachedGithubRepositories({ installationId, repositories: page.repositories, syncedAt });
      }
      const listedRepositoryIds = pages.flatMap((page) => page.repositories.map((repository) => repository.id));
      const deletedRepositoryCount = yield* pruneCachedGithubRepositories(installationId, listedRepositoryIds);
      return { processedPageCount: pages.length, processedRepoCount: listedRepositoryIds.length, deletedRepositoryCount };
    }));
  },
);

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
    // The unkeyed limit spans installations, so syncs holding a connection while they re-list leave most of a pool of 10.
    concurrency: [{ key: "event.data.installationId", limit: 1 }, { limit: 4 }],
  },
  async ({ event, step }) =>
    executeSyncGithubRepositories(
      { event, step },
      runInngestEffect,
    ),
  );
