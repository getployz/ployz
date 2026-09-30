import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import { preloadCollection } from "./query-collection";
import type { CollectionScope } from "./scope";
import { useCollectionScope } from "./use-collection-scope";
import { orgStoreTables } from "./collections";
/** The Org Store's single readiness signal: every table loaded. */
export function orgStoreOptions(organizationSlug: string, scope: CollectionScope) {
  return queryOptions({
    queryKey: ["org-store", scope.sessionId, scope.userId, organizationSlug],
    // Readiness happens once; each table keeps itself fresh after that.
    staleTime: Infinity,
    queryFn: async () => {
      await Promise.all(Object.values(orgStoreTables).map((get) => preloadCollection(get(organizationSlug, scope))));
      return true;
    },
  });
}

/** Suspends until the Org Store is ready. Only the dashboard shell's content gate calls this. */
export function useOrgStoreGate(organizationSlug: string) {
  useSuspenseQuery(orgStoreOptions(organizationSlug, useCollectionScope()));
}
