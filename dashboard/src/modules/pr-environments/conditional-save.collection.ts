import { useLiveSuspenseQuery } from "@tanstack/react-db";
import { getConditionalSavesCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEnvironmentDocuments } from "#/modules/environment-design/environment-document.collection";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { standing } from "./conditional-save";
import type { ConditionalSaveRow } from "./tables";

/** Every standing Conditional Save of the organization; withdrawn ones are left out. */
export function useStandingSaves(organizationSlug: string): ConditionalSaveRow[] {
  const { data } = useLiveSuspenseQuery(getConditionalSavesCollection(organizationSlug, useCollectionScope()));
  const environments = useEnvironmentDocuments(organizationSlug);
  const { branches } = useWorkspace(organizationSlug);
  return data.filter((save) => {
    const pr = environments.find((environment) => environment.id === save.prEnvironmentId);
    const branch = branches.find((candidate) => candidate.environmentId === save.prEnvironmentId);
    return standing(save, pr && { id: pr.id, revision: pr.revision, targetBranch: branch?.prTargetBranch ?? null });
  });
}

/** The standing Conditional Saves held on `destinationId`, oldest approval first. */
export function useHeldChanges(organizationSlug: string, destinationId: string) {
  return useStandingSaves(organizationSlug).filter((save) => save.destinationEnvironmentId === destinationId)
    .sort((a, b) => a.approvedAt.getTime() - b.approvedAt.getTime());
}
