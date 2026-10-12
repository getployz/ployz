import { queryOptions } from "@tanstack/react-query";
import { hasMarketingSiteServerFn } from "#/modules/marketing/marketing-site.functions";

/** Whether this Cloud serves the marketing site; a Self-hosted Cloud has none. */
export function marketingSiteQueryOptions() {
  return queryOptions({
    queryKey: ["marketing-site"],
    queryFn: ({ signal }) => hasMarketingSiteServerFn({ signal }),
    staleTime: Infinity,
  });
}
