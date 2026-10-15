import { queryOptions, useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { githubBuildRepositoryKey } from "#/modules/github/github-build-workflow";
import {
  listGithubBuildRepositoriesServerFn,
  getGithubRepoAccessStateServerFn,
  listGithubBranchesServerFn,
  getGithubWebServerFn,
  searchGithubFilesServerFn,
} from "#/modules/github/github.functions";

/** A repository as one Organization reads it: through its linked installation, or publicly. */
export type GithubRepositoryRef = {
  organizationSlug: string;
  repositoryId: number;
  installationId: number | null;
};

export function githubFileSearchQueryOptions(input: GithubRepositoryRef & {
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
  access: (organizationSlug: string) => [...githubKeys.all, "access", organizationSlug] as const,
  branches: (input: GithubRepositoryRef) =>
    [
      ...githubKeys.all,
      "branches",
      input.organizationSlug,
      input.installationId,
      input.repositoryId,
    ] as const,
  web: () => [...githubKeys.all, "web"] as const,
  accounts: (organizationSlug: string) => [...githubKeys.all, "accounts", organizationSlug] as const,
  offer: (organizationSlug: string, offer: string) => [...githubKeys.all, "offer", organizationSlug, offer] as const,
};

export function githubRepoAccessQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: githubKeys.access(organizationSlug),
    // Installation access changes in GitHub; recheck whenever a picker mounts.
    staleTime: 0,
    queryFn: () => getGithubRepoAccessStateServerFn({ data: { organizationSlug } }),
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

export function githubBranchesQueryOptions(input: GithubRepositoryRef) {
  return queryOptions({
    queryKey: githubKeys.branches(input),
    staleTime: 60_000,
    queryFn: () => listGithubBranchesServerFn({ data: input }),
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
