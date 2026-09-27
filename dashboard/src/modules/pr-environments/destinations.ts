import type { ServiceConfig } from "@ployz/sdk/config";

/** One of a project's Environments, by its latest Saved State. */
export type DestinationCandidate = {
  id: string;
  prEnvironment: boolean;
  savedServices: ReadonlyArray<Pick<ServiceConfig, "source">>;
};

function trackedBy(environment: DestinationCandidate, repositoryId: number) {
  return environment.savedServices.flatMap(({ source }) =>
    source.type === "git" && source.repositoryId === repositoryId && source.branch.type === "connected" ? [source.branch.name] : []);
}

/**
 * Where merges into `targetBranch` land: every Environment of the project, other than PR Environments, whose services from
 * the repository track that Git branch in its latest Saved State, whether or not they deploy on push.
 * `environments` are one project's.
 */
export function destinations(input: { environments: ReadonlyArray<DestinationCandidate>; repositoryId: number; targetBranch: string }) {
  return input.environments
    .filter((environment) => !environment.prEnvironment && trackedBy(environment, input.repositoryId).includes(input.targetBranch))
    .map((environment) => environment.id);
}

/** Every Git branch of the repository some Environment could be a Destination for, by name. */
export function trackedBranches(environments: ReadonlyArray<DestinationCandidate>, repositoryId: number) {
  return [...new Set(environments.filter((environment) => !environment.prEnvironment)
    .flatMap((environment) => trackedBy(environment, repositoryId)))].sort();
}
