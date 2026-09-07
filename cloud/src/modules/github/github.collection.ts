import { snakeCamelMapper } from "@electric-sql/client";
import {
  BasicIndex,
  type Collection,
  createCollection,
  createLiveQueryCollection,
} from "@tanstack/react-db";
import {
  electricCollectionOptions,
  type ElectricCollectionConfig,
} from "@tanstack/electric-db-collection";

import { tableSyncUrl } from "#/electric/table-sync-url";
import { plainRowCollection } from "#/lib/tanstack-db";
import type { GithubRepositorySelection } from "#/modules/github/github";
import { githubRepositoryCache as schemaGithubRepositoryCache } from "#/modules/github/tables";

type GithubRepositoryRow = typeof schemaGithubRepositoryCache.$inferSelect;
type GithubRepositoryView = GithubRepositorySelection & {
  user_id: string;
  synced_at: string;
};

let rawGithubRepos: ReturnType<typeof createRawGithubReposCollection> | undefined;

function createRawGithubReposCollection() {
  const config: ElectricCollectionConfig<GithubRepositoryRow> = {
    id: "electric:github_repository_cache",
    startSync: true,
    ["shapeOptions"]: {
      url: tableSyncUrl("github_repository_cache"),
      parser: {
        int8: (value) => Number(value),
        timestamptz: (value) => new Date(value),
        timestamp: (value) => new Date(value),
      },
      columnMapper: snakeCamelMapper(),
    },
    getKey: (row) => `${row.installationId}:${row.repositoryId}`,
    autoIndex: "eager",
    defaultIndexType: BasicIndex,
  };
  return createCollection(electricCollectionOptions(config));
}

export function getRawGithubReposCollection() {
  rawGithubRepos ??= createRawGithubReposCollection();
  return rawGithubRepos;
}

let githubRepos: ReturnType<typeof createGithubReposCollection> | undefined;

function createGithubReposCollection(): Collection<GithubRepositoryView> {
  return plainRowCollection(
    createLiveQueryCollection({
      id: "electric:github-repositories",
      startSync: true,
      query: (q) =>
        q.from({ repository: getRawGithubReposCollection() }).fn.select(
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

export function getGithubReposCollection() {
  githubRepos ??= createGithubReposCollection();
  return githubRepos;
}
