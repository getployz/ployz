import { useMutation } from "@tanstack/react-query";
import { getConditionalSavesCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import type { SavePick } from "#/modules/branches/branch-schemas";
import { approveConditionalSaveServerFn, giveConditionalSaveValueServerFn, withdrawConditionalSaveServerFn } from "./conditional-save-functions";

/**
 * Approve, Undo and a value given after approval, for one PR Environment and Destination. Awaited: the server recomputes
 * the rows and refuses a stale review, and seals new values. The Org Store row is read back before it counts as done.
 */
export function useConditionalSave(input: { organizationSlug: string; prEnvironmentId: string; destinationEnvironmentId: string }) {
  const saves = getConditionalSavesCollection(input.organizationSlug, useCollectionScope());
  const after = () => reconcileCollection(saves);
  return {
    approve: useMutation({
      mutationFn: async (approval: { review: string; picks: SavePick[] }) => {
        await approveConditionalSaveServerFn({ data: { ...input, ...approval } });
        await after();
      },
    }),
    withdraw: useMutation({
      mutationFn: async () => {
        await withdrawConditionalSaveServerFn({ data: input });
        await after();
      },
    }),
    give: useMutation({
      mutationFn: async (value: { key: string; value: string }) => {
        await giveConditionalSaveValueServerFn({ data: { ...input, ...value } });
        await after();
      },
    }),
  };
}
