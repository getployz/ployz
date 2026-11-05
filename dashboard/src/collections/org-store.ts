import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import { preloadCollections } from "./query-collection";
import type { CollectionScope } from "./scope";
import { useCollectionScope } from "./use-collection-scope";
import { orgStoreTables } from "./collections";
/** The Org Store's single readiness signal: every table loaded. */
export function orgStoreOptions(organizationSlug: string, scope: CollectionScope) {
  return queryOptions({
    queryKey: ["org-store", scope.sessionId, scope.userId, organizationSlug],
    // Readiness happens once; each table keeps itself fresh after that.
    staleTime: Infinity,
    // A table's first load fails at once, so the gate shows its error at once. Each table retries its own refreshes.
    retry: false,
    queryFn: async () => {
      await preloadCollections(Object.values(orgStoreTables).map((get) => get(organizationSlug, scope)));
      return true;
    },
  });
}

/** Suspends until the Org Store is ready. Only the dashboard shell's content gate calls this. */
export function useOrgStoreGate(organizationSlug: string) {
  useSuspenseQuery(orgStoreOptions(organizationSlug, useCollectionScope()));
}
