import { preloadCollection } from "#/collections/query-collection";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useSyncExternalStore } from "react";
import {
  queryOptions,
  useSuspenseQuery,
} from "@tanstack/react-query";
import {
  getEnvironmentDeploymentsCollection,
  getEnvironmentSavedStateRevisionsCollection,
} from "#/collections/collections";
import type { EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import { listLatestOrganizationEnvironmentChangeStatesServerFn } from "#/modules/deployments/deployment.functions";
import { serviceDeploymentKeys } from "#/modules/deployments/deployment-queries";

type EnvironmentChangeStateProjectionInput = {
  organizationSlug: string;
  environmentId: string | null;
};

function projectionMetadata(organizationSlug: string, scope: CollectionScope) {
  return {
    deployments: getEnvironmentDeploymentsCollection(organizationSlug, scope),
    savedStateRevisions: getEnvironmentSavedStateRevisionsCollection(organizationSlug, scope),
  };
}

type ProjectionMetadata = ReturnType<typeof projectionMetadata>;

function readProjectionVersion({ deployments, savedStateRevisions }: ProjectionMetadata) {
  return projectionVersion([...deployments.values()], [...savedStateRevisions.values()]);
}

export function environmentChangeStateOptions(organizationSlug: string, scope: CollectionScope) {
  const version = readProjectionVersion(projectionMetadata(organizationSlug, scope));
  return queryOptions({
    queryKey: [
      ...serviceDeploymentKeys.environmentChangeStatesOrg(organizationSlug),
      scope.sessionId, scope.userId, scope.environmentSlug ?? null, version,
    ],
    staleTime: Infinity,
    queryFn: () => listLatestOrganizationEnvironmentChangeStatesServerFn({
      data: { organizationSlug, environmentSlug: scope.environmentSlug },
    }),
  });
}

export async function preloadOrganizationEnvironmentChangeStateProjections(scope: CollectionScope, organizationSlug: string) {
  const metadata = projectionMetadata(organizationSlug, scope);
  await Promise.all(Object.values(metadata).map(preloadCollection));
  return scope.queryClient.ensureQueryData(environmentChangeStateOptions(organizationSlug, scope));
}

function projectionVersion(deployments: Array<{ id: string; status: string; updatedAt: Date }>, revisions: Array<{ id: string }>) {
  return [
    ...deployments.map((row) => `deployment:${row.id}:${row.status}:${row.updatedAt.getTime()}`),
    ...revisions.map((row) => `saved:${row.id}`),
  ].sort().join("|");
}

/**
 * Owns the complete read seam for one environment's explicit change state.
 *
 * Saved and Applied State are projected on the server. Saved revision inserts
 * and deployment lifecycle changes arrive through metadata-only API
 * collections, so each metadata version has one shared projection query.
 */
export function useEnvironmentChangeStateProjection({
  organizationSlug,
  environmentId,
}: EnvironmentChangeStateProjectionInput): EnvironmentChangeStateProjection | null {
  const scope = useCollectionScope();
  useEnvironmentProjectionVersion(projectionMetadata(organizationSlug, scope));
  const { data: organizationState } = useSuspenseQuery(environmentChangeStateOptions(organizationSlug, scope));
  return environmentId
    ? (organizationState.find(
        (state) => state.environmentId === environmentId,
      ) ?? null)
    : null;
}

/** Read the same hydrated collection snapshot as the loader; subscribe only for subsequent changes. */
export function useEnvironmentProjectionVersion(metadata: ProjectionMetadata) {
  return useSyncExternalStore(
    (onChange) => {
      const subscriptions = Object.values(metadata).map((collection) => collection.subscribeChanges(onChange));
      return () => subscriptions.forEach((subscription) => subscription.unsubscribe());
    },
    () => readProjectionVersion(metadata),
    () => readProjectionVersion(metadata),
  );
}
