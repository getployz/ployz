import { toast } from "sonner";
import { createOptimisticAction } from "@tanstack/react-db";
import { getBranchesCollection, getEnvironmentsCollection } from "#/collections/collections";
import { observeFailure } from "#/collections/query-collection";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { SetupCommand } from "#/modules/project/tables";
import { setBranchKeptServerFn } from "./branch-close.functions";
import { setBranchSetupDefaultsServerFn } from "./branch-functions";

const getKeepBranchAction = cachedByCollectionScope((organizationSlug, scope) => {
  const branches = getBranchesCollection(organizationSlug, scope);
  return createOptimisticAction<{ environmentId: string; kept: boolean }>({
    onMutate: ({ environmentId, kept }) => branches.update(environmentId, (draft) => { draft.kept = kept; }),
    mutationFn: async ({ environmentId, kept }) => {
      try {
        await branches.writeCommitted(await setBranchKeptServerFn({ data: { organizationSlug, environmentId, kept } }));
      } catch (error) {
        toast.error(error instanceof Error ? error.message : "Could not change whether this branch is kept.");
        throw error;
      }
    },
  });
});

/** Keeps a Branch, or stops keeping it; applies at once and rolls back on failure. */
export function useKeepBranch(organizationSlug: string) {
  const keep = getKeepBranchAction(organizationSlug, useCollectionScope());
  return (environmentId: string, kept: boolean) => observeFailure(keep({ environmentId, kept }));
}

const getBranchSetupDefaultsAction = cachedByCollectionScope((organizationSlug, scope) => {
  const environments = getEnvironmentsCollection(organizationSlug, scope);
  return createOptimisticAction<{ environmentId: string; setupCommands: SetupCommand[] }>({
    onMutate: ({ environmentId, setupCommands }) => environments.update(environmentId, (draft) => { draft.branchSetupCommands = setupCommands; }),
    mutationFn: async (data) => {
      try {
        await environments.writeCommitted(await setBranchSetupDefaultsServerFn({ data: { organizationSlug, ...data } }));
      } catch (error) {
        toast.error(error instanceof Error ? error.message : "Could not save the setup commands.");
        throw error;
      }
    },
  });
});

/** Saves the Setup Commands that prefill new Branches of an Environment; applies at once and rolls back on failure. */
export function useBranchSetupDefaults(organizationSlug: string) {
  const save = getBranchSetupDefaultsAction(organizationSlug, useCollectionScope());
  return (environmentId: string, setupCommands: SetupCommand[]) => observeFailure(save({ environmentId, setupCommands }));
}
