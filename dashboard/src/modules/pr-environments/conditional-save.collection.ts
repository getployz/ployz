import { useLiveQuery } from "@tanstack/react-db";
import { getConditionalSavesCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentChangeStateProjection } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocuments } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { standing } from "./conditional-save";
import type { ConditionalSaveRow } from "./tables";

/** Every Conditional Save of the organization, with whether it still stands. Nonsuspending: the shell's crumbs read it. */
export function useConditionalSaves(organizationSlug: string): Array<ConditionalSaveRow & { standing: boolean }> {
  const { data } = useLiveQuery(getConditionalSavesCollection(organizationSlug, useCollectionScope()));
  const environments = useEnvironmentDocuments(organizationSlug);
  const { branches } = useWorkspace(organizationSlug);
  return data.map((save) => {
    const pr = environments.find((environment) => environment.id === save.prEnvironmentId);
    const pullRequest = branches.find((candidate) => candidate.environmentId === save.prEnvironmentId)?.pullRequest;
    return { ...save, standing: standing(save, pr && pullRequest && { id: pr.id, revision: pr.revision, targetBranch: pullRequest.targetBranch }) };
  });
}

/** The Conditional Saves held on `destinationId`, standing or frozen at the merge and not landed yet, oldest approval first. */
export function useHeldChanges(organizationSlug: string, destinationId: string): ConditionalSaveRow[] {
  return useConditionalSaves(organizationSlug)
    .filter((save) => save.destinationEnvironmentId === destinationId && (save.standing || save.state === "frozen"))
    .sort((a, b) => a.approvedAt.getTime() - b.approvedAt.getTime());
}

/** Landed Conditional Saves whose rows `destinationId` had changed since approval, so they weren't saved there. */
export function useStagedInstead(organizationSlug: string, destinationId: string) {
  const savedStateId = useEnvironmentChangeStateProjection({ organizationSlug, environmentId: destinationId })?.saved?.snapshotId;
  return useConditionalSaves(organizationSlug)
    .filter((save) => save.destinationEnvironmentId === destinationId && save.state === "landed" && save.landedSavedStateId === savedStateId);
}
