import { queryOptions, useQuery } from "@tanstack/react-query";
import { githubBuildRepositoryKey } from "#/modules/github/github-build-workflow";
import {
  listGithubBuildRepositoriesServerFn,
  getGithubRepoAccessStateServerFn,
  listGithubBranchesServerFn,
  getGithubInstallUrlServerFn,
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
  installUrl: () => [...githubKeys.all, "install-url"] as const,
};

export function githubRepoAccessQueryOptions() {
  return queryOptions({
    queryKey: githubKeys.access(),
    // Installation access changes in GitHub; recheck whenever a picker mounts.
    staleTime: 0,
    queryFn: () => getGithubRepoAccessStateServerFn(),
  });
}

export function githubInstallUrlQueryOptions() {
  return queryOptions({
    queryKey: githubKeys.installUrl(),
    queryFn: () => getGithubInstallUrlServerFn(),
    staleTime: Infinity,
  });
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
    // The workflow lands in GitHub, usually from another tab: refetch on focus, and poll while one is awaited.
    staleTime: 30_000,
    refetchOnWindowFocus: "always",
  });
}

/** Repositories the organization builds from, polled every 10s while one in `opened` still needs its workflow. */
export function useGithubBuildRepositories(organizationSlug: string, opened: ReadonlySet<string> = new Set()) {
  // Not suspense: a GitHub failure must not take the Servers page down with it.
  return useQuery({
    ...githubBuildRepositoriesQueryOptions(organizationSlug),
    refetchInterval: (query) =>
      query.state.data?.some((repository) => repository.readiness === "setup_needed" && opened.has(githubBuildRepositoryKey(repository)))
        ? 10_000
        : false,
  });
}
