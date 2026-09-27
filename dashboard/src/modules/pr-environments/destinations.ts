import type { ServiceConfig } from "@ployz/sdk/config";
import type { EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import { prEnvironmentIds, trackedBranch } from "./pull-request";

/** One of a project's Environments, by its latest Saved State. */
export type DestinationCandidate = {
  id: string;
  prEnvironment: boolean;
  savedServices: ReadonlyArray<Pick<ServiceConfig, "source">>;
};

function trackedBy(environment: DestinationCandidate, repositoryId: number) {
  return environment.savedServices.flatMap((config) => trackedBranch(config, repositoryId) ?? []);
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

/** Each Environment as a Destination candidate in the browser, by its change state's latest Saved services. */
export function destinationCandidates(
  environments: ReadonlyArray<{ id: string }>,
  branches: Parameters<typeof prEnvironmentIds>[0],
  states: ReadonlyArray<Pick<EnvironmentChangeStateProjection, "environmentId" | "saved">>,
): DestinationCandidate[] {
  const prEnvironments = prEnvironmentIds(branches);
  return environments.map((environment) => ({
    id: environment.id,
    prEnvironment: prEnvironments.has(environment.id),
    savedServices: states.find((state) => state.environmentId === environment.id)?.saved?.nodes
      .flatMap((node) => node.nodeType === "service" ? [node.config] : []) ?? [],
  }));
}
