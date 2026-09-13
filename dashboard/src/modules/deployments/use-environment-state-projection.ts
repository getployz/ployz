import { preloadCollection } from "#/collections/query-collection";
import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { useEffect } from "react";
import { eq, useLiveQuery } from "@tanstack/react-db";
import {
  queryOptions,
  useQueryClient,
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

function organizationEnvironmentChangeStatesQueryOptions(
  organizationSlug: string,
) {
  return queryOptions({
    queryKey:
      serviceDeploymentKeys.environmentChangeStatesOrg(organizationSlug),
    queryFn: () =>
      listLatestOrganizationEnvironmentChangeStatesServerFn({
        data: { organizationSlug },
      }),
  });
}

export function preloadOrganizationEnvironmentChangeStateProjections(
  scope: CollectionScope,
  organizationSlug: string,
) {
  return Promise.all([
    preloadCollection(getEnvironmentDeploymentsCollection(organizationSlug, scope)),
    preloadCollection(getEnvironmentSavedStateRevisionsCollection(organizationSlug, scope)),
    scope.queryClient.ensureQueryData(
      organizationEnvironmentChangeStatesQueryOptions(organizationSlug),
    ),
  ]);
}

/**
 * Owns the complete read seam for one environment's explicit change state.
 *
 * Saved and Applied State are projected on the server. Saved revision inserts
 * and deployment lifecycle changes arrive through metadata-only API
 * collections, so every authoritative projection change invalidates this query.
 */
export function useEnvironmentChangeStateProjection({
  organizationSlug,
  environmentId,
}: EnvironmentChangeStateProjectionInput): EnvironmentChangeStateProjection | null {
  const scope = useCollectionScope();
  const deployments = getEnvironmentDeploymentsCollection(organizationSlug, scope);
  const savedStateRevisions =
    getEnvironmentSavedStateRevisionsCollection(organizationSlug, scope);
  useEnvironmentProjectionRefresh({ organizationSlug, environmentId }, { deployments, savedStateRevisions });
  const { data: organizationState } = useSuspenseQuery(
    organizationEnvironmentChangeStatesQueryOptions(organizationSlug),
  );
  return environmentId
    ? (organizationState.find(
        (state) => state.environmentId === environmentId,
      ) ?? null)
    : null;
}

/** Metadata joins drive refresh independently of the projection's server read. */
export function useEnvironmentProjectionRefresh(
  { organizationSlug, environmentId }: EnvironmentChangeStateProjectionInput,
  { deployments, savedStateRevisions }: {
    deployments: ReturnType<typeof getEnvironmentDeploymentsCollection>;
    savedStateRevisions: ReturnType<typeof getEnvironmentSavedStateRevisionsCollection>;
  },
) {
  const queryClient = useQueryClient();
  const { data: deploymentLifecycle = [] } = useLiveQuery(
    (q) => {
      if (!environmentId) return undefined;
      return q
        .from({ deployment: deployments })
        .where(({ deployment }) => eq(deployment.environmentId, environmentId))
        .select(({ deployment }) => ({
          id: deployment.id,
          status: deployment.status,
          updatedAt: deployment.updatedAt,
        }));
    },
    [deployments, environmentId],
  );
  const { data: savedRevisions = [] } = useLiveQuery(
    (q) => {
      if (!environmentId) return undefined;
      return q
        .from({ savedRevision: savedStateRevisions })
        .where(({ savedRevision }) =>
          eq(savedRevision.environmentId, environmentId),
        )
        .select(({ savedRevision }) => ({ id: savedRevision.id }));
    },
    [savedStateRevisions, environmentId],
  );
  const projectionVersion = [
    ...deploymentLifecycle.map(
      (deployment) =>
        `deployment:${deployment.id}:${deployment.status}:${deployment.updatedAt.getTime()}`,
    ),
    ...savedRevisions.map((revision) => `saved:${revision.id}`),
  ]
    .sort()
    .join("|");

  useEffect(() => {
    if (!environmentId) return;
    void queryClient.invalidateQueries({
      queryKey:
        serviceDeploymentKeys.environmentChangeStatesOrg(organizationSlug),
    });
  }, [environmentId, organizationSlug, projectionVersion, queryClient]);

}
