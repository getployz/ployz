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

export function pullRequestColumns(pullRequest: PullRequestFacts) {
  return {
    prRepositoryId: pullRequest.repositoryId,
    prRepository: pullRequest.repository,
    prNumber: pullRequest.number,
    prTitle: pullRequest.title,
    prAuthor: pullRequest.author,
    prHeadBranch: pullRequest.headBranch,
    prHeadSha: pullRequest.headSha,
    prTargetBranch: pullRequest.targetBranch,
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
