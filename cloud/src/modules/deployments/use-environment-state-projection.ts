import { useEffect } from "react";
import { eq, useLiveQuery } from "@tanstack/react-db";
import {
  queryOptions,
  type QueryClient,
  useQueryClient,
  useSuspenseQuery,
} from "@tanstack/react-query";
import {
  getEnvironmentDeploymentsCollection,
  getEnvironmentSavedStateRevisionsCollection,
} from "#/electric/collections";
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
  queryClient: QueryClient,
  organizationSlug: string,
) {
  return Promise.all([
    getEnvironmentDeploymentsCollection(organizationSlug).preload(),
    getEnvironmentSavedStateRevisionsCollection(organizationSlug).preload(),
    queryClient.ensureQueryData(
      organizationEnvironmentChangeStatesQueryOptions(organizationSlug),
    ),
  ]);
}

/**
 * Owns the complete read seam for one environment's explicit change state.
 *
 * Saved and Applied State are projected on the server. Saved revision inserts
 * and deployment lifecycle changes arrive through metadata-only Electric
 * Shapes, so every authoritative projection change invalidates this query.
 */
export function useEnvironmentChangeStateProjection({
  organizationSlug,
  environmentId,
}: EnvironmentChangeStateProjectionInput): EnvironmentChangeStateProjection | null {
  const queryClient = useQueryClient();
  const deployments = getEnvironmentDeploymentsCollection(organizationSlug);
  const savedStateRevisions =
    getEnvironmentSavedStateRevisionsCollection(organizationSlug);
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
  const { data: organizationState } = useSuspenseQuery(
    organizationEnvironmentChangeStatesQueryOptions(organizationSlug),
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

  return environmentId
    ? (organizationState.find(
        (state) => state.environmentId === environmentId,
      ) ?? null)
    : null;
}
