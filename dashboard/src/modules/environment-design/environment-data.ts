import { queryOptions } from "@tanstack/react-query";
import type { CollectionScope } from "#/collections/scope";
import { preloadCollection } from "#/collections/query-collection";
import {
  getProjectsCollection, getEnvironmentsCollection, getRawServicesCollection,
  getRawEnvironmentResourcesCollection, getResourceLineagesCollection,
  getCanvasPositionsCollection, getEnvironmentNodeConfigSnapshotsCollection,
  getVolumeRemoveAttemptsCollection,
  getEnvironmentNodeIntroductionsCollection,
} from "#/collections/collections";
import { preloadOrganizationEnvironmentChangeStateProjections } from "#/modules/deployments/use-environment-state-projection";
import {
  getServicesCollection, getEnvironmentResourcesCollection, getVolumeResourcesCollection,
  type EnvironmentParams,
} from "#/modules/services/services.collection";

export function environmentResourcesOptions(params: EnvironmentParams, scope: CollectionScope) {
  const { organizationSlug, projectSlug, environmentSlug } = params;
  return queryOptions({
    queryKey: [
      "environment-resources",
      scope.sessionId,
      scope.userId,
      organizationSlug,
      projectSlug,
      environmentSlug,
    ],
    staleTime: Infinity,
    queryFn: async () => {
      // Reuse lifecycle projections: a Volume remains navigable until removal completes.
      await Promise.all([
        preloadCollection(getProjectsCollection(organizationSlug, scope)),
        preloadCollection(getEnvironmentsCollection(organizationSlug, scope)),
        preloadCollection(getRawServicesCollection(organizationSlug, scope)),
        preloadCollection(
          getRawEnvironmentResourcesCollection(organizationSlug, scope),
        ),
        preloadCollection(
          getResourceLineagesCollection(organizationSlug, scope),
        ),
        preloadCollection(
          getCanvasPositionsCollection(organizationSlug, scope),
        ),
        preloadCollection(
          getEnvironmentNodeConfigSnapshotsCollection(organizationSlug, scope),
        ),
        preloadCollection(
          getVolumeRemoveAttemptsCollection(organizationSlug, scope),
        ),
      ]);
      await Promise.all([
        getServicesCollection(organizationSlug, scope).preload(),
        getEnvironmentResourcesCollection(organizationSlug, scope).preload(),
        getVolumeResourcesCollection(organizationSlug, scope).preload(),
      ]);
      return true;
    },
  });
}

export function environmentCanvasOptions(params: EnvironmentParams, scope: CollectionScope) {
  const resources = environmentResourcesOptions(params, scope);
  return queryOptions({
    queryKey: ["environment-canvas", ...resources.queryKey.slice(1)],
    staleTime: Infinity,
    queryFn: async () => {
      await Promise.all([
        scope.queryClient.ensureQueryData(resources),
        preloadCollection(getEnvironmentNodeIntroductionsCollection(params.organizationSlug, scope)),
        preloadOrganizationEnvironmentChangeStateProjections(scope, params.organizationSlug),
      ]);
      return true;
    },
  });
}
