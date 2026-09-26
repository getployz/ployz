import type { CollectionScope } from "#/collections/scope";
import { useCollectionScope } from "#/collections/use-collection-scope";
import {
  queryOptions,
  useSuspenseQuery,
} from "@tanstack/react-query";
import type { EnvironmentChangeStateProjection } from "#/modules/deployments/deployment-contract";
import { listLatestOrganizationEnvironmentChangeStatesServerFn } from "#/modules/deployments/deployment.functions";
import { serviceDeploymentKeys } from "#/modules/deployments/deployment-queries";

type EnvironmentChangeStateProjectionInput = {
  organizationSlug: string;
  environmentId: string | null;
};

type ReadChangeStates = (input: Parameters<typeof listLatestOrganizationEnvironmentChangeStatesServerFn>[0]) => ReturnType<typeof listLatestOrganizationEnvironmentChangeStatesServerFn>;

export function environmentChangeStateOptions(organizationSlug: string, scope: CollectionScope,
  read: ReadChangeStates = listLatestOrganizationEnvironmentChangeStatesServerFn,
) {
  return queryOptions({
    queryKey: [
      ...serviceDeploymentKeys.environmentChangeStatesOrg(organizationSlug),
      scope.sessionId, scope.userId,
    ],
    // The Organization change stream invalidates it when deployment rows or saved revisions change.
    staleTime: Infinity,
    queryFn: ({ signal }) => read({ data: { organizationSlug }, signal }),
  });
}

export async function preloadOrganizationEnvironmentChangeStateProjections(scope: CollectionScope, organizationSlug: string) {
  return scope.queryClient.ensureQueryData(environmentChangeStateOptions(organizationSlug, scope));
}

/** The change stream named `environment_change_state`: refetch every scoped copy of the projection. */
export function refetchEnvironmentChangeStates(organizationSlug: string, scope: CollectionScope) {
  void scope.queryClient.invalidateQueries({ queryKey: serviceDeploymentKeys.environmentChangeStatesOrg(organizationSlug) });
}

/**
 * Owns the complete read seam for one environment's explicit change state.
 *
 * Saved and Applied State are projected on the server. The Organization change
 * log names `environment_change_state` when a deployment row or saved revision
 * changes; progress events never do. Refreshes retain the existing result so an
 * open editor never suspends on background work.
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

/** Share one scoped comparison read, retaining its data during refreshes. */
export function useEnvironmentChangeStates(organizationSlug: string, scope: CollectionScope,
  read: ReadChangeStates = listLatestOrganizationEnvironmentChangeStatesServerFn,
) {
  return useSuspenseQuery(environmentChangeStateOptions(organizationSlug, scope, read)).data;
}
