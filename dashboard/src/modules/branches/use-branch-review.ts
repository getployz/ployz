import { useEnvironmentChangeStatesIfReady } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocuments } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { destinationCandidates, destinations } from "#/modules/pr-environments/destinations";
import { useConditionalSaves } from "#/modules/pr-environments/conditional-save.collection";
import { prCheck, type PrCheck } from "#/modules/pr-environments/pr-check";
import type { ConditionalSaveRow } from "#/modules/pr-environments/tables";
import { branchHostnameSuffix } from "./branch-plan";
import { useLineageNames } from "./use-lineage-names";
import { branchReview, goesTo, latestDeploy, liveUpdates, usedLive, variableName, type BranchReview, type GoesTo, type LiveUpdate } from "./branch-review";

export type PullRequest = { number: number; title: string; author: string; headBranch: string; targetBranch: string };

type EnvironmentName = { id: string; name: string; namespace: string };

export type BranchReviewView = BranchReview & {
  /** The Destination of a Merge: the Parent. */
  parent: EnvironmentName;
  kept: boolean;
  live: LiveUpdate[];
  /** A PR Environment's pull request; null for any other Branch. */
  pullRequest: PullRequest | null;
  /** A PR Environment's changes for each of its Destinations, which it never Merges into. Empty for any other Branch. */
  goesTo: Array<GoesTo & { destination: EnvironmentName; approval: ConditionalSaveRow | null }>;
  /** A PR Environment whose every Destination with changes has a standing approval. */
  approved: boolean;
  /** A PR Environment's "Ployz · ready to merge" check, as Ployz posts it on the pull request; null for any other Branch. */
  check: PrCheck | null;
  /** "N changes": what would merge, or on a PR Environment what goes to its Destinations. */
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
  const { projects, branches, environments: summaries } = useWorkspace(organizationSlug);
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
  const saves = useConditionalSaves(organizationSlug);

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
    const pr = row.prNumber === null || row.prRepositoryId === null ? null : {
      number: row.prNumber, title: row.prTitle ?? "", author: row.prAuthor ?? "",
      headBranch: row.prHeadBranch ?? "", targetBranch: row.prTargetBranch ?? "",
    };
    // Oldest first, as the Environments tree lists them.
    const project = summaries.filter((environment) => environment.projectId === branch.projectId)
      .sort((a, b) => a.createdAt.getTime() - b.createdAt.getTime());
    const landings = pr && row.prRepositoryId !== null ? destinations({
      environments: destinationCandidates(project, branches, states), repositoryId: row.prRepositoryId, targetBranch: pr.targetBranch,
    }).flatMap((id) => {
      const destination = environmentById.get(id);
      const save = saves.find((candidate) => candidate.prEnvironmentId === environmentId && candidate.destinationEnvironmentId === id) ?? null;
      return destination ? [{
        destination,
        save,
        approval: save?.standing ? save : null,
        ...goesTo({
          base: row.base, kept: false, branch: branch.intent, parent: destination.intent,
          parentApplied: stateById.get(parent.id)?.applied.intent ?? null,
          hostnames: { branch: hostnameSuffix(branch.id), parent: hostnameSuffix(id) },
        }),
      }] : [];
    }) : [];
    const check = pr && prCheck(landings.map(({ destination, rows, save }) => ({
      name: destination.name,
      changes: rows.length,
      approval: save && {
        standing: save.standing, changes: save.rows.length, approvedBy: save.approvedBy,
        missing: save.rows.filter((held) => held.missing).map((held) => variableName(held.row)),
      },
    })));
    return {
      ...review, live, parent, kept: row.kept, pullRequest: pr, goesTo: landings, check,
      approved: landings.some((landing) => landing.rows.length) && landings.every((landing) => !landing.rows.length || landing.approval),
      changes: pr ? landings.reduce((sum, landing) => sum + landing.rows.length, 0) : review.merge.length,
      updates: review.update.length + live.length,
      nameOf: (lineage) => lineageName(lineage, environmentId), environmentName: (id) => environmentById.get(id)?.name ?? "another environment",
    };
  };
}

export function useBranchReview(organizationSlug: string, environmentId: string) {
  return useBranchReviews(organizationSlug)(environmentId);
}
