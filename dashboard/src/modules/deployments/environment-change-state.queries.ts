import { preloadCollection } from "#/collections/query-collection";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEffect, useSyncExternalStore } from "react";
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

type ReadChangeStates = (input: Parameters<typeof listLatestOrganizationEnvironmentChangeStatesServerFn>[0]) => ReturnType<typeof listLatestOrganizationEnvironmentChangeStatesServerFn>;

export function environmentChangeStateOptions(organizationSlug: string, scope: CollectionScope,
  read: ReadChangeStates = listLatestOrganizationEnvironmentChangeStatesServerFn,
) {
  return queryOptions({
    queryKey: [
      ...serviceDeploymentKeys.environmentChangeStatesOrg(organizationSlug),
      scope.sessionId, scope.userId,
    ],
    staleTime: Infinity,
    queryFn: async ({ signal }) => {
      const version = readProjectionVersion(projectionMetadata(organizationSlug, scope));
      const states = await read({ data: { organizationSlug }, signal });
      return { version, states };
    },
  });
}

export async function preloadOrganizationEnvironmentChangeStateProjections(scope: CollectionScope, organizationSlug: string) {
  const metadata = projectionMetadata(organizationSlug, scope);
  await Promise.all(Object.values(metadata).map(preloadCollection));
  return scope.queryClient.ensureQueryData(environmentChangeStateOptions(organizationSlug, scope));
}

function projectionVersion(deployments: Array<{ id: string; status: string; savedStateSnapshotId: string }>, revisions: Array<{ id: string }>) {
  return [
    ...deployments.map((row) => `deployment:${row.id}:${row.status}:${row.savedStateSnapshotId}`),
    ...revisions.map((row) => `saved:${row.id}`),
  ].sort().join("|");
}

/**
 * Owns the complete read seam for one environment's explicit change state.
 *
 * Saved and Applied State are projected on the server. Saved revision inserts
 * and deployment lifecycle changes arrive through metadata-only API
 * collections. Progress timestamps do not affect comparison state, and refreshes
 * retain the existing result so an open editor never suspends on background work.
 */
export function useEnvironmentChangeStateProjection({
  organizationSlug,
  environmentId,
}: EnvironmentChangeStateProjectionInput): EnvironmentChangeStateProjection | null {
  const scope = useCollectionScope();
  const organizationState = useEnvironmentChangeStates(organizationSlug, scope);
  return environmentId
    ? (organizationState.find(
        (state) => state.environmentId === environmentId,
      ) ?? null)
    : null;
}

/** Share one scoped comparison read, retaining its data during metadata refreshes. */
export function useEnvironmentChangeStates(organizationSlug: string, scope: CollectionScope,
  read: ReadChangeStates = listLatestOrganizationEnvironmentChangeStatesServerFn,
) {
  const version = useEnvironmentProjectionVersion(projectionMetadata(organizationSlug, scope));
  const options = environmentChangeStateOptions(organizationSlug, scope, read);
  const { data, dataUpdatedAt } = useSuspenseQuery(options);
  const { queryClient, sessionId, userId } = scope;
  useEffect(() => {
    // A manual refresh can finish with the cached version after newer metadata arrived.
    if (data.version !== version) {
      const { queryKey } = environmentChangeStateOptions(organizationSlug, { queryClient, sessionId, userId });
      void queryClient.invalidateQueries({ queryKey, exact: true }, { cancelRefetch: false });
    }
  }, [data.version, dataUpdatedAt, version, organizationSlug, queryClient, sessionId, userId]);
  return data.states;
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
