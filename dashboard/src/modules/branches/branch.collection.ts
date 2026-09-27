import { toast } from "sonner";
import { createOptimisticAction } from "@tanstack/react-db";
import { getBranchesCollection } from "#/collections/collections";
import { observeFailure } from "#/collections/query-collection";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { setBranchKeptServerFn } from "./branch-close.functions";

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
