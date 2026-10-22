import { useQuery } from "@tanstack/react-query";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";

/** Whether this Cloud seals variables, so a Server without its Cluster's Sealing Key is worth naming. */
export function useSealedVariables(organizationSlug: string) {
  // Set per deploy, so this read never refetches the Organization state on its own.
  return useQuery({ ...organizationStateQueryOptions(organizationSlug), staleTime: Infinity, select: (state) => state.sealedVariables }).data ?? false;
}
