import { useQuery } from "@tanstack/react-query";
import { organizationStateQueryOptions } from "#/modules/organization/organization-state.queries";

export function useSealedVariables(organizationSlug: string) {
  return useQuery({ ...organizationStateQueryOptions(organizationSlug), staleTime: Infinity, select: (state) => state.sealedVariables }).data ?? false;
}
