import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useLoaderData } from "@tanstack/react-router";
import { toast } from "sonner";
import { getSupportAccessCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { applySupportAccessChange, SUPPORT_SHOWS_NONE, type SupportAccessChange, type SupportAccessRow } from "./support-access";
import { setSupportAccessServerFn } from "./support-access.functions";

/**
 * Whether Ployz support may open the Organization, and what it may read inside. A change shows at once, saves in the
 * background, and rolls back with a toast.
 */
export function useSupportAccess(organizationSlug: string) {
  const collection = getSupportAccessCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery(collection);
  const { organizationId } = useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
  const none: SupportAccessRow = { id: organizationId, allowed: false, ...SUPPORT_SHOWS_NONE };
  const row = rows[0] ?? none;
  const set = createOptimisticAction<SupportAccessChange>({
    onMutate: (change) => {
      const current = collection.get(organizationId);
      const next = applySupportAccessChange(current ?? none, change);
      if (current === undefined) collection.insert(next);
      else collection.update(organizationId, (draft) => { Object.assign(draft, next); });
    },
    mutationFn: (change) => setSupportAccessServerFn({ data: { organizationSlug, change } })
      .then((saved) => collection.writeCommitted(saved)),
  });
  return {
    row,
    change: (change: SupportAccessChange) => {
      set(change).isPersisted.promise.catch(() => toast.error("Could not change support access"));
    },
  };
}
