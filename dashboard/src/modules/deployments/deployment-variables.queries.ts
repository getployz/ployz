import { queryOptions } from "@tanstack/react-query";
import { getDeploymentServiceVariablesServerFn } from "./deployment.functions";

/** A service's variables as the attempt deployed them; null is a sealed value. */
export function deploymentServiceVariablesQueryOptions(organizationSlug: string, deploymentId: string, serviceId: string) {
  return queryOptions({
    queryKey: ["deployment-service-variables", organizationSlug, deploymentId, serviceId],
    // An attempt's frozen inputs never change, so neither does what it deployed.
    staleTime: Infinity,
    queryFn: ({ signal }) => getDeploymentServiceVariablesServerFn({ data: { organizationSlug, deploymentId, serviceId }, signal }),
  });
}
