/** @data-source remote: each Server's latest web traffic change, the latest-of over history computed on the server; the change stream refetches it when a change's row changes */
import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import { listLatestServerWebTrafficServerFn } from "./server-web-traffic.functions";
import { defineChange } from "#/collections/change-log/declare";

/** Each Server's latest web traffic change; the change stream's `server_web_traffic` invalidates this root. */
export const serverWebTrafficKeys = { all: ["server-web-traffic"] as const };

export const serverWebTrafficChange = defineChange({
  name: "server_web_traffic",
  sources: ["server_web_traffic_attempt"],
  invalidate: (_slug, scope) => void scope.queryClient.invalidateQueries({ queryKey: serverWebTrafficKeys.all }),
});

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
