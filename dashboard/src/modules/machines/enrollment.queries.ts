/** @data-source remote: whether the first Server has founded the Cluster, polled every 5s only while a tab holds a Cloudflare token for it */
import { queryOptions } from "@tanstack/react-query";
import { clusterFoundedServerFn } from "#/modules/machines/enrollment.functions";

export function clusterFoundedQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: ["cluster-founded", organizationSlug] as const,
    queryFn: () => clusterFoundedServerFn({ data: { organizationSlug } }),
    staleTime: 5_000,
    refetchInterval: 5_000,
  });
}
