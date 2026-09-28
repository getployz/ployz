import { useMutation } from "@tanstack/react-query";
import { toast } from "sonner";
import { getConditionalSavesCollection, getEnvironmentsCollection } from "#/collections/collections";
import { reconcileCollection } from "#/collections/query-collection";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { refetchEnvironmentChangeStates } from "#/modules/deployments/environment-change-state.queries";
import { useEnvironmentDocumentQueue } from "#/modules/environment-design/environment-document-edit";
import type { SavePick } from "#/modules/branches/branch-schemas";
import {
  saveConditionalSaveServerFn, takePullRequestValueServerFn, withdrawConditionalSaveServerFn,
} from "./conditional-save-functions";

/**
 * Save and Undo, for one PR Environment and Destination. Awaited: the server recomputes the rows and refuses a stale
 * review, and seals new values. Pending edits to the PR Environment save first, so the review matches them. The Org
 * Store row is read back before it counts as done.
 */
export function useConditionalSave(scope: { organizationSlug: string; prEnvironmentId: string; destinationEnvironmentId: string }) {
  const saves = getConditionalSavesCollection(scope.organizationSlug, useCollectionScope());
  const queue = useEnvironmentDocumentQueue(scope.organizationSlug);
  return {
    save: useMutation({
      mutationFn: async (save: { review: string; picks: SavePick[] }) => {
        await queue.settled(scope.prEnvironmentId);
        await saveConditionalSaveServerFn({ data: { ...scope, ...save } });
        await reconcileCollection(saves);
      },
    }),
    withdraw: useMutation({
      mutationFn: async () => {
        await withdrawConditionalSaveServerFn({ data: scope });
        await reconcileCollection(saves);
      },
      onSuccess: () => toast.success("Undone"),
      onError: (error) => toast.error(error.message),
    }),
  };
}

/** "Use": a merged pull request's value replaces the Environment's own undeployed edit, as a change to deploy. */
export function useTakePullRequestValue(organizationSlug: string, environmentId: string) {
  const scope = useCollectionScope();
  const queue = useEnvironmentDocumentQueue(organizationSlug);
  return useMutation({
    mutationFn: async (input: { conditionalSaveId: string; key: string }) => {
      await queue.settled(environmentId);
      await takePullRequestValueServerFn({ data: { organizationSlug, ...input } });
      await Promise.all([getEnvironmentsCollection, getConditionalSavesCollection].map((get) => reconcileCollection(get(organizationSlug, scope))));
      refetchEnvironmentChangeStates(organizationSlug, scope);
    },
    onError: (error) => toast.error(error.message),
  });
}
