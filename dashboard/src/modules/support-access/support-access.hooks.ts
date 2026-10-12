import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useLoaderData } from "@tanstack/react-router";
import { toast } from "sonner";
import { getSupportAccessCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { setSupportAccessServerFn } from "./support-access.functions";

/** Whether Ployz support may open the Organization. A change shows at once, saves in the background, and rolls back with a toast. */
export function useSupportAccess(organizationSlug: string) {
  const collection = getSupportAccessCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery(collection);
  const { organizationId } = useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
  const set = createOptimisticAction<boolean>({
    onMutate: (allowed) => {
      if (collection.has(organizationId)) collection.update(organizationId, (row) => { row.allowed = allowed; });
      else collection.insert({ id: organizationId, allowed });
    },
    mutationFn: (allowed) => setSupportAccessServerFn({ data: { organizationSlug, allowed } })
      .then((row) => collection.writeCommitted(row)),
  });
  return {
    allowed: rows[0]?.allowed ?? false,
    setAllowed: (allowed: boolean) => {
      set(allowed).isPersisted.promise.catch(() => toast.error("Could not change support access"));
    },
  };
}
