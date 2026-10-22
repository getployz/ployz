import { Data, Effect, Schema } from "effect";
import { listInstallationReposPage } from "#/modules/github/github.api";
import type { GithubRepository } from "#/modules/github/github";
import type { GithubApi } from "#/modules/github/github-observation.api";
import {
  lockGithubRepositoryCache,
  markGithubRepositoriesSynced,
  pruneCachedGithubRepositories,
  readGithubRepositoryCacheVersion,
  upsertCachedGithubRepositories,
} from "#/modules/github/github.repository";
import { Database } from "#/server/database.server";
import type { AppConfig } from "#/server/config.server";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { githubRepositoriesSyncRequestedEventType } from "#/modules/inngest/events";
import { runInngestEffect } from "#/server/run.server";

/**
 * A request replaces the installation's running sync rather than being skipped: a run that already wrote may still be
 * finishing, and would never see what prompted the request. A write the replaced run still commits bumps the cache
 * version, so the new run lists again rather than writing over it.
 */
export const SYNC_GITHUB_REPOSITORIES_SINGLETON = {
  key: "event.data.installationId",
  mode: "cancel",
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

/** Listings a sync takes before it gives up to a retry, each set aside because another writer changed the cache. */
const SYNC_LISTING_ATTEMPTS = 3;

export class GithubRepositoryCacheBusy extends Data.TaggedError("GithubRepositoryCacheBusy")<{
  readonly installationId: number;
}> {}

/** How long GitHub may take to list one page, so a listing it stops answering fails to a retry. GitHub ends any request it takes over 10 seconds to serve. */
export const GITHUB_REPOSITORY_PAGE_DEADLINE = "30 seconds";

export class GithubRepositoryListingStalled extends Data.TaggedError("GithubRepositoryListingStalled")<{
  readonly installationId: number;
  readonly page: number;
}> {
  override get message() {
    return `GitHub didn't list page ${this.page} of installation ${this.installationId}'s repositories within ${GITHUB_REPOSITORY_PAGE_DEADLINE}.`;
  }
}

/** What a sync of an installation GitHub or Cloud forgot reports: it wrote nothing. */
const FORGOTTEN = { processedPageCount: 0, processedRepoCount: 0, deletedRepositoryCount: 0, forgotten: true } as const;

/**
 * Lists every repository GitHub grants the installation, holding no connection or lock, then caches them and drops the
 * rest in one short transaction under the installation's cache lock. If another writer changed the cache since the
 * listing began, the listing may be older than that write, so it is listed again rather than written. Once the
 * installation is forgotten there is nothing to write, so the sync stops. A page GitHub doesn't list within its deadline
 * fails the sync, writing nothing, and its retry lists again from the first page.
 */
export const syncGithubRepositories = Effect.fn("Github.syncRepositories")(function* (installationId: number) {
  const database = yield* Database;
  for (let attempt = 1; attempt <= SYNC_LISTING_ATTEMPTS; attempt++) {
    const listedVersion = yield* readGithubRepositoryCacheVersion(installationId);
    if (listedVersion === null) return FORGOTTEN;
    const syncedAt = new Date();
    const repositories: GithubRepository[] = [];
    let processedPageCount = 0;
    for (let pageNumber = 1; ; pageNumber++) {
      const page = yield* listInstallationReposPage(installationId, pageNumber).pipe(
        Effect.timeoutOrElse({
          duration: GITHUB_REPOSITORY_PAGE_DEADLINE,
          orElse: () => Effect.fail(new GithubRepositoryListingStalled({ installationId, page: pageNumber })),
        }),
        Effect.catch((error) =>
          readGithubRepositoryCacheVersion(installationId).pipe(
            Effect.flatMap((version) => version === null ? Effect.succeed(null) : Effect.fail(error)))));
      if (page === null) return FORGOTTEN;
      repositories.push(...page.repositories);
      processedPageCount += 1;
      if (!page.hasNextPage) break;
    }
    const written = yield* database.transaction(Effect.gen(function* () {
      const version = yield* lockGithubRepositoryCache(installationId);
      if (version === null) return FORGOTTEN;
      if (version !== listedVersion) return null;
      yield* upsertCachedGithubRepositories({ installationId, repositories, syncedAt });
      const deletedRepositoryCount = yield* pruneCachedGithubRepositories(installationId, repositories.map((repository) => repository.id));
      yield* markGithubRepositoriesSynced(installationId, syncedAt);
      return { processedPageCount, processedRepoCount: repositories.length, deletedRepositoryCount, forgotten: false };
    }));
    if (written !== null) return written;
  }
  return yield* new GithubRepositoryCacheBusy({ installationId });
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
    concurrency: [{ key: "event.data.installationId", limit: 1 }],
  },
  async ({ event, step }) =>
    executeSyncGithubRepositories(
      { event, step },
      runInngestEffect,
    ),
  );
