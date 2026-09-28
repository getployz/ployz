import type { ServiceConfig } from "@ployz/sdk/config";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";

type GitSource = Extract<ServiceConfig["source"], { type: "git" }>;

/** Whether a service deploys from the repository. */
export function fromRepository(config: Pick<ServiceConfig, "source">, repositoryId: number): config is { source: GitSource } {
  return config.source.type === "git" && config.source.repositoryId === repositoryId;
}

/** The Git branch of the repository a service tracks, or null. */
export function trackedBranch(config: Pick<ServiceConfig, "source">, repositoryId: number) {
  return fromRepository(config, repositoryId) && config.source.branch.type === "connected" ? config.source.branch.name : null;
}

/** A PR Environment's derived configuration: the repository's services track the head Git branch; every Own Copy runs one replica. */
export function prEnvironmentIntent(intent: SavedEnvironmentIntent, pullRequest: { repositoryId: number; headBranch: string }): SavedEnvironmentIntent {
  const tracking = trackingHead(intent, pullRequest);
  return { ...tracking, services: tracking.services.map((node) => ({ ...node, config: { ...node.config, replicas: 1 } })) };
}

/** `intent` with the repository's services tracking the pull request's head Git branch. */
export function trackingHead(intent: SavedEnvironmentIntent, pullRequest: { repositoryId: number; headBranch: string }): SavedEnvironmentIntent {
  return {
    ...intent,
    services: intent.services.map((node) => fromRepository(node.config, pullRequest.repositoryId)
      ? { ...node, config: { ...node.config, source: { ...node.config.source, branch: { type: "connected", name: pullRequest.headBranch } } } }
      : node),
  };
}

export type PrBranch = { environmentId: string; projectId: string; pullRequest: { repositoryId: number; number: number; closed: boolean; retired: boolean } | null };

/** The PR Environments among `branches`, by Environment id. */
export function prEnvironmentIds(branches: ReadonlyArray<PrBranch>) {
  return new Set(branches.flatMap((branch) => branch.pullRequest ? [branch.environmentId] : []));
}

/** The project's PR Environments for one repository whose pull request is open, by pull request number. */
export function openPrEnvironments<B extends PrBranch>(branches: ReadonlyArray<B>, projectId: string, repositoryId: number) {
  return branches.filter((branch) => branch.projectId === projectId && branch.pullRequest?.repositoryId === repositoryId && !branch.pullRequest.closed && !branch.pullRequest.retired)
    .sort((a, b) => (a.pullRequest?.number ?? 0) - (b.pullRequest?.number ?? 0));
}
