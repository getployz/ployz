import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { useLoaderData } from "@tanstack/react-router";
import { toast } from "sonner";
import { getOrganizationSettingsCollection } from "#/collections/collections";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { organizationSettings } from "#/modules/approvals/approvals";
import { setOrganizationSettingsServerFn } from "#/modules/approvals/approvals.functions";

/** "Ask before destructive actions": a change shows at once, saves in the background, and rolls back with a toast. */
export function useAskBeforeDestructive(organizationSlug: string) {
  const collection = getOrganizationSettingsCollection(organizationSlug, useCollectionScope());
  const { data: rows } = useLiveSuspenseQuery(collection);
  const { organizationId } = useLoaderData({ from: "/_protected/cloud/$organizationSlug" });
  const { askBeforeDestructive } = organizationSettings(rows);
  const set = createOptimisticAction<boolean>({
    onMutate: (ask) => {
      if (collection.has(organizationId)) collection.update(organizationId, (row) => { row.askBeforeDestructive = ask; });
      else collection.insert({ id: organizationId, askBeforeDestructive: ask });
    },
    mutationFn: (ask) => setOrganizationSettingsServerFn({ data: { organizationSlug, askBeforeDestructive: ask } })
      .then((row) => collection.writeCommitted(row)),
  });
  return {
    askBeforeDestructive,
    setAskBeforeDestructive: (ask: boolean) => {
      set(ask).isPersisted.promise.catch(() => toast.error("Could not change asking before destructive actions"));
    },
  };
}
