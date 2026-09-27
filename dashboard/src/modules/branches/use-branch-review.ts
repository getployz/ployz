import { useEnvironmentChangeStatesIfReady } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocuments } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { branchHostnameSuffix } from "./branch-plan";
import { useLineageNames } from "./use-lineage-names";
import { branchReview, latestDeploy, liveUpdates, usedLive, type BranchReview, type LiveUpdate } from "./branch-review";

export type BranchReviewView = BranchReview & {
  /** The Destination: the Parent. */
  parent: { id: string; name: string; namespace: string };
  kept: boolean;
  live: LiveUpdate[];
  /** "N changes": what would merge. */
  changes: number;
  /** "N updates": the Parent's deployed changes the Branch lacks, plus Live Nodes redeployed since. */
  updates: number;
  nameOf: (lineage: string) => string;
  environmentName: (environmentId: string) => string;
};

/**
 * Each Branch's review, computed in the browser with core's branchChanges from Org Store rows. The returned function
 * computes on call, so a list pays only for the Branches it shows. Null for a root, or until the rows land.
 */
export function useBranchReviews(organizationSlug: string): (environmentId: string) => BranchReviewView | null {
  const { projects, branches } = useWorkspace(organizationSlug);
  const environments = useEnvironmentDocuments(organizationSlug);
  const states = useEnvironmentChangeStatesIfReady(organizationSlug);
  const environmentById = new Map(environments.map((environment) => [environment.id, environment]));
  const branchById = new Map(branches.map((branch) => [branch.environmentId, branch]));
  const stateById = new Map((states ?? []).map((state) => [state.environmentId, state]));
  const deployedAt = new Map((states ?? []).map((state) => [state.environmentId, state.applied.deployedAt]));

  const hostnameSuffix = (environmentId: string) => {
    const environment = environmentById.get(environmentId);
    const project = projects.find((candidate) => candidate.id === environment?.projectId);
    return environment && project ? branchHostnameSuffix(project.slug, environment.namespace, branchById.has(environmentId)) : "";
  };
  const lineageName = useLineageNames(organizationSlug);

  return (environmentId) => {
    const row = branchById.get(environmentId);
    const branch = environmentById.get(environmentId);
    const parent = row && environmentById.get(row.parentEnvironmentId);
    if (!row || !branch || !parent || !states) return null;
    const review = branchReview({
      base: row.base, kept: row.kept, branch: branch.intent, parent: parent.intent,
      parentApplied: stateById.get(parent.id)?.applied.intent ?? null,
      hostnames: { branch: hostnameSuffix(branch.id), parent: hostnameSuffix(parent.id) },
    });
    const live = liveUpdates({ live: usedLive(branch.intent), parentId: parent.id, branches, branchDeployedAt: latestDeploy(deployedAt.get(environmentId)), deployedAt });
    return {
      ...review, live, parent, kept: row.kept,
      changes: review.merge.length, updates: review.update.length + live.length,
      nameOf: (lineage) => lineageName(lineage, environmentId), environmentName: (id) => environmentById.get(id)?.name ?? "another environment",
    };
  };
}

export function useBranchReview(organizationSlug: string, environmentId: string) {
  return useBranchReviews(organizationSlug)(environmentId);
}
