import { queryOptions, useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { preloadCollection } from "./query-collection";
import type { CollectionScope } from "./scope";
import { useCollectionScope } from "./use-collection-scope";
import * as collections from "./collections";
import { preloadOrganizationEnvironmentChangeStateProjections } from "#/modules/deployments/environment-change-state.queries";
import { getOrganizationDeploymentsCollection } from "#/modules/deployments/deployment.collection";
import { getEnvironmentDocumentsCollection } from "#/modules/environment-design/environment-document.collection";
import {
  getServicesCollection, getVolumeResourcesCollection,
} from "#/modules/services/services.collection";

/** Every table getter `collections.ts` exports is an Org Store table. */
// SAFETY: the name filter keeps only cachedByCollectionScope table getters, which all take (organizationSlug, scope) and return an API collection.
const orgStoreTables = Object.entries(collections)
  .filter(([name]) => /^get\w+Collection$/.test(name))
  .map(([, get]) => get as typeof collections.getProjectsCollection);

/** Every derived view in an org-store data file. Adding a view means adding it here. */
export const orgStoreViews = [
  getEnvironmentDocumentsCollection, getServicesCollection, getVolumeResourcesCollection, getOrganizationDeploymentsCollection,
];

/** Every server projection in an org-store Query file. Adding a projection means adding its preload here. */
export const orgStoreProjections = [preloadOrganizationEnvironmentChangeStateProjections];

/**
 * The Org Store's single readiness signal. Tables load together; derived views
 * and the change-state projection start once their raw rows exist.
 */
export function orgStoreOptions(organizationSlug: string, scope: CollectionScope) {
  return queryOptions({
    queryKey: ["org-store", scope.sessionId, scope.userId, organizationSlug],
    // Readiness happens once; each table keeps itself fresh after that.
    staleTime: Infinity,
    queryFn: async () => {
      await Promise.all(orgStoreTables.map((get) => preloadCollection(get(organizationSlug, scope))));
      await Promise.all([
        ...orgStoreViews.map((get) => get(organizationSlug, scope).preload()),
        // ponytail: change state waits on deployment metadata to stamp its version; parallel once the server returns the version.
        ...orgStoreProjections.map((preload) => preload(scope, organizationSlug)),
      ]);
      return true;
    },
  });
}

/** Suspends until the Org Store is ready. Only the dashboard shell's content gate calls this. */
export function useOrgStoreGate(organizationSlug: string) {
  useSuspenseQuery(orgStoreOptions(organizationSlug, useCollectionScope()));
}

/** Non-suspending readiness for chrome that renders outside the content gate. */
export function useOrgStoreStatus(organizationSlug: string) {
  return useQuery(orgStoreOptions(organizationSlug, useCollectionScope()));
}
