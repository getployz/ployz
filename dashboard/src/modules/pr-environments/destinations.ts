import type { ServiceConfig } from "@ployz/sdk/config";
import type { EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import { prEnvironmentIds, trackedBranch } from "./pull-request";

/** One of a project's Environments, by its latest Saved State. */
export type DestinationCandidate = {
  id: string;
  parentId: string | null;
  prEnvironment: boolean;
  savedServices: ReadonlyArray<Pick<ServiceConfig, "source">>;
};

function trackedBy(environment: DestinationCandidate, repositoryId: number) {
  return environment.savedServices.flatMap((config) => trackedBranch(config, repositoryId) ?? []);
}

/**
 * Where merges into `targetBranch` land: every Environment of the project, other than PR Environments, whose services from
 * the repository track that Git branch in its latest Saved State, whether or not they deploy on push, with none in its
 * Parent chain tracking it too. `environments` are one project's.
 */
export function destinations(input: { environments: ReadonlyArray<DestinationCandidate>; repositoryId: number; targetBranch: string }) {
  const parentOf = new Map(input.environments.map((environment) => [environment.id, environment.parentId]));
  const tracking = new Set(input.environments
    .filter((environment) => !environment.prEnvironment && trackedBy(environment, input.repositoryId).includes(input.targetBranch))
    .map((environment) => environment.id));
  const trackedAbove = (id: string) => {
    for (let parent = parentOf.get(id); parent; parent = parentOf.get(parent)) if (tracking.has(parent)) return true;
    return false;
  };
  return [...tracking].filter((id) => !trackedAbove(id));
}

/** Every Git branch of the repository some Environment could be a Destination for, by name. */
export function trackedBranches(environments: ReadonlyArray<DestinationCandidate>, repositoryId: number) {
  return [...new Set(environments.filter((environment) => !environment.prEnvironment)
    .flatMap((environment) => trackedBy(environment, repositoryId)))].sort();
}

/** Each Environment as a Destination candidate in the browser, by its change state's latest Saved services. */
export function destinationCandidates(
  environments: ReadonlyArray<{ id: string }>,
  branches: ReadonlyArray<Parameters<typeof prEnvironmentIds>[0][number] & { parentEnvironmentId: string }>,
  states: ReadonlyArray<Pick<EnvironmentChangeStateProjection, "environmentId" | "saved">>,
): DestinationCandidate[] {
  const prEnvironments = prEnvironmentIds(branches);
  return environments.map((environment) => ({
    id: environment.id,
    parentId: branches.find((branch) => branch.environmentId === environment.id)?.parentEnvironmentId ?? null,
    prEnvironment: prEnvironments.has(environment.id),
    savedServices: states.find((state) => state.environmentId === environment.id)?.saved?.nodes
      .flatMap((node) => node.nodeType === "service" ? [node.config] : []) ?? [],
  }));
}
