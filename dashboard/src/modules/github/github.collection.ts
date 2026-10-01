import { githubInstallUrlQueryOptions, githubRepoAccessQueryOptions } from "./github.queries";
import { skipToken, useQuery, type QueryClient } from "@tanstack/react-query";
import { createApiCollection, preloadCollection } from "#/collections/query-collection";
import type { GithubRepositorySelection } from "#/modules/github/github";
import { listGithubRepositoriesServerFn } from "#/modules/github/github.functions";
import { githubRepositoryCache as schemaGithubRepositoryCache } from "#/modules/github/tables";

type GithubRepositoryRow = typeof schemaGithubRepositoryCache.$inferSelect;
export type GithubRepositoryView = GithubRepositorySelection & {
  user_id: string;
  synced_at: string;
};

export type GithubCollectionScope = {
  queryClient: QueryClient;
  userId: string;
  sessionId: string;
};

const scopes = new WeakMap<QueryClient, Map<string, ReturnType<typeof createGithubReposCollection>>>();

export function githubReposQueryKey(scope: GithubCollectionScope) {
  return ["collections", scope.sessionId, scope.userId, "github_repository_cache"];
}

function toGithubRepositoryView(repository: GithubRepositoryRow): GithubRepositoryView {
  return {
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
  };
}

function createGithubReposCollection(scope: GithubCollectionScope) {
  return createApiCollection({
    queryClient: scope.queryClient,
    queryKey: githubReposQueryKey(scope),
    queryFn: async ({ signal }) => (await listGithubRepositoriesServerFn({ signal })).map(toGithubRepositoryView),
    getKey: (row: GithubRepositoryView) => `${row.installation_id}:${row.id}`,
    // Reopening a picker within a minute reuses the cache.
    staleTime: 60_000,
    // A requested sync lands rows in the background; poll only while a picker holds the collection.
    refetchInterval: 15_000,
  });
}

export function getGithubReposCollection(scope: GithubCollectionScope) {
  let cache = scopes.get(scope.queryClient);
  if (!cache) {
    cache = new Map();
    scopes.set(scope.queryClient, cache);
  }
  const key = `${scope.sessionId}:${scope.userId}`;
  let collection = cache.get(key);
  if (!collection) {
    collection = createGithubReposCollection(scope);
    cache.set(key, collection);
  }
  return collection;
}

/** Query state of the repository read; failed refreshes keep rows, so errors repaint from here. */
export function useGithubReposReadState(scope: GithubCollectionScope) {
  return useQuery({ queryKey: githubReposQueryKey(scope), queryFn: skipToken });
}

/**
 * Start a picker's reads together (repositories, access, install URL) when it opens or is about to;
 * failures surface through `useGithubReposReadState` and the picker's own queries.
 */
export function preloadGithubRepos(scope: GithubCollectionScope) {
  void preloadCollection(getGithubReposCollection(scope)).catch(() => {});
  void scope.queryClient.prefetchQuery(githubRepoAccessQueryOptions());
  void scope.queryClient.prefetchQuery(githubInstallUrlQueryOptions());
}
