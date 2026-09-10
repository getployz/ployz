import {
  type Collection,
  createLiveQueryCollection,
} from "@tanstack/react-db";
import type { QueryClient } from "@tanstack/react-query";
import { createApiCollection } from "#/collections/query-collection";
import { readCollectionServerFn } from "#/collections/read.functions";
import { plainRowCollection } from "#/lib/tanstack-db";
import type { GithubRepositorySelection } from "#/modules/github/github";
import { githubRepositoryCache as schemaGithubRepositoryCache } from "#/modules/github/tables";

type GithubRepositoryRow = typeof schemaGithubRepositoryCache.$inferSelect;
type GithubRepositoryView = GithubRepositorySelection & {
  user_id: string;
  synced_at: string;
};

export type GithubCollectionScope = {
  queryClient: QueryClient;
  userId: string;
  sessionId: string;
};

const scopes = new WeakMap<QueryClient, Map<string, {
  raw: ReturnType<typeof createRawGithubReposCollection>;
  view?: Collection<GithubRepositoryView>;
}>>();

export function githubReposQueryKey(scope: GithubCollectionScope) {
  return ["collections", scope.sessionId, scope.userId, "github_repository_cache"];
}

function createRawGithubReposCollection(scope: GithubCollectionScope) {
  return createApiCollection<GithubRepositoryRow>({
    queryClient: scope.queryClient,
    queryKey: githubReposQueryKey(scope),
    queryFn: async ({ signal }) => {
      const rows = await readCollectionServerFn({
        data: { table: "github_repository_cache", userId: scope.userId },
        signal,
      });
      // SAFETY: the literal table selects githubRepositoryCache in the allowlisted read.
      return rows as GithubRepositoryRow[];
    },
    getKey: (row) => `${row.installationId}:${row.repositoryId}`,
  });
}

function getScope(scope: GithubCollectionScope) {
  let cache = scopes.get(scope.queryClient);
  if (!cache) {
    cache = new Map();
    scopes.set(scope.queryClient, cache);
  }
  const key = `${scope.sessionId}:${scope.userId}`;
  let entry = cache.get(key);
  if (!entry) {
    entry = { raw: createRawGithubReposCollection(scope) };
    cache.set(key, entry);
  }
  return entry;
}

export function getRawGithubReposCollection(scope: GithubCollectionScope) {
  return getScope(scope).raw;
}

export function createGithubReposCollection(raw: Collection<GithubRepositoryRow>, id: string): Collection<GithubRepositoryView> {
  return plainRowCollection(
    createLiveQueryCollection({
      id,
      gcTime: 1,
      query: (q) =>
        q.from({ repository: raw }).fn.select(
          ({ repository }): GithubRepositoryView => ({
            id: repository.repositoryId,
            installation_id: repository.installationId,
            name: repository.name,
            full_name: repository.fullName,
            default_branch: repository.defaultBranch,
            private: repository.private,
            html_url: repository.htmlUrl,
            repo_updated_at: repository.repoUpdatedAt.toISOString(),
            user_id: repository.userId,
            synced_at: repository.syncedAt.toISOString(),
          }),
        ),
      getKey: (row: GithubRepositoryView) =>
        `${row.installation_id}:${row.id}`,
    }),
  );
}

export function getGithubReposCollection(scope: GithubCollectionScope) {
  const entry = getScope(scope);
  entry.view ??= createGithubReposCollection(entry.raw, `api:${scope.sessionId}:github-repositories`);
  return entry.view;
}
