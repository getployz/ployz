import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import { listLatestServerWebTrafficServerFn } from "./server-web-traffic.functions";

/** Each Server's latest web traffic change; the change stream's `server_web_traffic` invalidates this root. */
export const serverWebTrafficKeys = { all: ["server-web-traffic"] as const };

/** Each Server's latest web traffic change, keyed by Machine ID. The change stream refetches it when a row changes. */
export function latestServerWebTrafficQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...serverWebTrafficKeys.all, organizationSlug, "latest"] as const,
    queryFn: () => listLatestServerWebTrafficServerFn({ data: { organizationSlug } }),
    staleTime: 60_000,
  });
}

/** Every Server's latest web traffic change. The Server page's loader prefetches it. */
export function useServerWebTrafficChanges(organizationSlug: string) {
  return useSuspenseQuery(latestServerWebTrafficQueryOptions(organizationSlug)).data;
}
