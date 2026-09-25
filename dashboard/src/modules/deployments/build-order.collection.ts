import { toast } from "sonner";
import { createOptimisticAction, useLiveSuspenseQuery } from "@tanstack/react-db";
import { cachedByCollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { getBuildOrderCollection } from "#/collections/collections";
import { observeFailure } from "#/collections/query-collection";
import { setBuildOrderServerFn } from "./build-order.functions";
import type { BuildOrder } from "./build-order";

const getBuildOrderEditor = cachedByCollectionScope((organizationSlug, scope) => {
  const rows = getBuildOrderCollection(organizationSlug, scope);
  return createOptimisticAction<{ organizationId: string; buildOrder: BuildOrder }>({
    onMutate: ({ organizationId, buildOrder }) => rows.update(organizationId, (draft) => { draft.buildOrder = buildOrder; }),
    mutationFn: async ({ buildOrder }) => {
      try {
        await rows.writeCommitted(await setBuildOrderServerFn({ data: { organizationSlug, buildOrder } }));
      } catch (error) {
        toast.error(error instanceof Error ? error.message : "Could not save the build order.");
        throw error;
      }
    },
  });
});

/** The Organization's chosen Build Order (null: the default applies) and its immediate, never-staged setter. */
export function useBuildOrder(organizationSlug: string) {
  const scope = useCollectionScope();
  const { data: [row] } = useLiveSuspenseQuery(getBuildOrderCollection(organizationSlug, scope));
  const edit = getBuildOrderEditor(organizationSlug, scope);
  return {
    buildOrder: row?.buildOrder ?? null,
    setBuildOrder: (buildOrder: BuildOrder) => {
      if (row) observeFailure(edit({ organizationId: row.id, buildOrder }));
    },
  };
}
