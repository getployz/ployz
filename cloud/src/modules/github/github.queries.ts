import { queryOptions } from "@tanstack/react-query";
import {
  getGithubRepoAccessStateServerFn,
  listGithubBranchesServerFn,
  getGithubInstallUrlServerFn,
} from "#/modules/github/github.functions";

export const githubKeys = {
  all: ["github"] as const,
  repos: () => [...githubKeys.all, "repos"] as const,
  access: () => [...githubKeys.all, "access"] as const,
  branches: (input: {
    repositoryFullName: string;
    repositoryId: number;
    installationId: number;
  }) =>
    [
      ...githubKeys.all,
      "branches",
      input.installationId,
      input.repositoryId,
      input.repositoryFullName,
    ] as const,
  installUrl: () => [...githubKeys.all, "install-url"] as const,
};

export function githubRepoAccessQueryOptions() {
  return queryOptions({
    queryKey: githubKeys.access(),
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
  repositoryFullName: string;
  repositoryId: number;
  installationId: number;
}) {
  return queryOptions({
    queryKey: githubKeys.branches(input),
    queryFn: () =>
      listGithubBranchesServerFn({
        data: {
          repositoryId: input.repositoryId,
          installationId: input.installationId,
        },
      }),
  });
}
