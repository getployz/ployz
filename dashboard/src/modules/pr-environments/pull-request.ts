import type { ServiceConfig } from "@ployz/sdk/config";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";

/** What a PR Environment's Branch row records about its pull request. */
export type PullRequestFacts = {
  repositoryId: number;
  repository: string;
  number: number;
  title: string;
  author: string;
  headBranch: string;
  headSha: string;
  targetBranch: string;
};

/** The Branch row's pull request columns: all null on a Branch without one. */
export function pullRequestColumns(pullRequest: PullRequestFacts | undefined) {
  return {
    prRepositoryId: pullRequest?.repositoryId ?? null,
    prRepository: pullRequest?.repository ?? null,
    prNumber: pullRequest?.number ?? null,
    prTitle: pullRequest?.title ?? null,
    prAuthor: pullRequest?.author ?? null,
    prHeadBranch: pullRequest?.headBranch ?? null,
    prHeadSha: pullRequest?.headSha ?? null,
    prTargetBranch: pullRequest?.targetBranch ?? null,
  };
}

export function fromRepository(config: Pick<ServiceConfig, "source">, repositoryId: number) {
  return config.source.type === "git" && config.source.repositoryId === repositoryId;
}

/** A PR Environment's derived configuration: the repository's services track the head Git branch; every Own Copy runs one replica. */
export function prEnvironmentIntent(intent: SavedEnvironmentIntent, pullRequest: PullRequestFacts): SavedEnvironmentIntent {
  return {
    ...intent,
    services: intent.services.map((node) => {
      const { source } = node.config;
      return {
        ...node,
        config: {
          ...node.config,
          replicas: 1,
          source: source.type === "git" && source.repositoryId === pullRequest.repositoryId
            ? { ...source, branch: { type: "connected", name: pullRequest.headBranch } }
            : source,
        },
      };
    }),
  };
}

type PrBranch = { projectId: string; prRepositoryId: number | null; prNumber: number | null };

/** The project's open PR Environments for one repository, by pull request number. */
export function openPrEnvironments<B extends PrBranch>(branches: ReadonlyArray<B>, projectId: string, repositoryId: number) {
  return branches.filter((branch) => branch.projectId === projectId && branch.prRepositoryId === repositoryId)
    .sort((a, b) => (a.prNumber ?? 0) - (b.prNumber ?? 0));
}
