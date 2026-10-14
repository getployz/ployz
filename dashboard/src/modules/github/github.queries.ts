import { queryOptions, useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { githubBuildRepositoryKey } from "#/modules/github/github-build-workflow";
import {
  listGithubBuildRepositoriesServerFn,
  getGithubRepoAccessStateServerFn,
  listGithubBranchesServerFn,
  getGithubWebServerFn,
  searchGithubFilesServerFn,
} from "#/modules/github/github.functions";

export function githubFileSearchQueryOptions(input: {
  repositoryId: number;
  installationId: number | null;
  ref: string;
  pattern: string;
}) {
  return queryOptions({
    queryKey: [...githubKeys.all, "files", input],
    queryFn: () => searchGithubFilesServerFn({ data: input }),
    staleTime: 60_000,
  });
}

export const githubKeys = {
  all: ["github"] as const,
  repos: () => [...githubKeys.all, "repos"] as const,
  access: () => [...githubKeys.all, "access"] as const,
  branches: (input: {
    repositoryId: number;
    installationId: number | null;
  }) =>
    [
      ...githubKeys.all,
      "branches",
      input.installationId,
      input.repositoryId,
    ] as const,
  web: () => [...githubKeys.all, "web"] as const,
};

export function githubRepoAccessQueryOptions() {
  return queryOptions({
    queryKey: githubKeys.access(),
    // Installation access changes in GitHub; recheck whenever a picker mounts.
    staleTime: 0,
    queryFn: () => getGithubRepoAccessStateServerFn(),
  });
}

export function githubWebQueryOptions() {
  return queryOptions({
    queryKey: githubKeys.web(),
    queryFn: () => getGithubWebServerFn(),
    staleTime: Infinity,
  });
}

/** GitHub's web origin for browser links; every Organization page's loader reads it first. */
export function useGithubWebOrigin() {
  return useSuspenseQuery(githubWebQueryOptions()).data.origin;
}

export function githubBranchesQueryOptions(input: {
  repositoryId: number;
  installationId: number | null;
}) {
  return queryOptions({
    queryKey: githubKeys.branches(input),
    staleTime: 60_000,
    queryFn: () =>
      listGithubBranchesServerFn({
        data: {
          repositoryId: input.repositoryId,
          installationId: input.installationId,
        },
      }),
  });
}

export function githubBuildRepositoriesQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...githubKeys.all, "build-repositories", organizationSlug] as const,
    queryFn: () => listGithubBuildRepositoriesServerFn({ data: { organizationSlug } }),
    // Each read calls GitHub per repository: focus refetches only once stale; an awaited workflow polls instead.
    staleTime: 30_000,
  });
}

/** Repositories the organization builds from, polled every 10s while one in `opened` still needs its workflow. */
export function useGithubBuildRepositories(organizationSlug: string, opened: ReadonlySet<string> = new Set()) {
  // Not suspense: a GitHub failure must not take Organization › Builds down with it.
  return useQuery({
    ...githubBuildRepositoriesQueryOptions(organizationSlug),
    refetchInterval: (query) =>
      query.state.data?.some((repository) => repository.readiness === "setup_needed" && opened.has(githubBuildRepositoryKey(repository)))
        ? 10_000
        : false,
  });
}
