import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { getConditionalSavesCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocuments } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { standing } from "./conditional-save";
import type { ConditionalSaveRow } from "./tables";

function useConditionalSaves(organizationSlug: string) {
  return useLiveSuspenseQuery(getConditionalSavesCollection(organizationSlug, useCollectionScope())).data;
}

/** Every standing Conditional Save of the organization; withdrawn ones are left out. */
export function useStandingSaves(organizationSlug: string): ConditionalSaveRow[] {
  const data = useConditionalSaves(organizationSlug);
  const environments = useEnvironmentDocuments(organizationSlug);
  const { branches } = useWorkspace(organizationSlug);
  return data.filter((save) => {
    const pr = environments.find((environment) => environment.id === save.prEnvironmentId);
    const branch = branches.find((candidate) => candidate.environmentId === save.prEnvironmentId);
    return standing(save, pr && { id: pr.id, revision: pr.revision, targetBranch: branch?.prTargetBranch ?? null });
  });
}

/** The Conditional Saves held on `destinationId`, standing or frozen at the merge and not landed yet, oldest approval first. */
export function useHeldChanges(organizationSlug: string, destinationId: string) {
  const frozen = useConditionalSaves(organizationSlug).filter((save) => save.mergeCommitSha !== null && save.landedSavedStateId === null);
  return [...useStandingSaves(organizationSlug), ...frozen].filter((save) => save.destinationEnvironmentId === destinationId)
    .sort((a, b) => a.approvedAt.getTime() - b.approvedAt.getTime());
}

/** Landed Conditional Saves whose rows `destinationId` had changed since approval, so they're staged there, not saved. */
export function useStagedInstead(organizationSlug: string, destinationId: string) {
  const savedStateId = useEnvironmentChangeStateProjection({ organizationSlug, environmentId: destinationId })?.saved?.snapshotId;
  return useConditionalSaves(organizationSlug)
    .filter((save) => save.destinationEnvironmentId === destinationId && save.landedSavedStateId !== null && save.landedSavedStateId === savedStateId);
}
