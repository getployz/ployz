import { queryOptions, useSuspenseQuery } from "@tanstack/react-query";
import { listLatestServerDrainsServerFn } from "./server-drain.functions";

/** Each Server's latest Drain; the change stream's `server_drain` invalidates this root. */
export const serverDrainKeys = { all: ["server-drain"] as const };

/** Each Server's latest Drain, keyed by Machine ID. The change stream refetches it when a Drain's row changes. */
export function latestServerDrainsQueryOptions(organizationSlug: string) {
  return queryOptions({
    queryKey: [...serverDrainKeys.all, organizationSlug, "latest"] as const,
    queryFn: () => listLatestServerDrainsServerFn({ data: { organizationSlug } }),
    staleTime: 60_000,
  });
}

/** Every Server's latest Drain. The Server page's loader prefetches it. */
export function useServerDrains(organizationSlug: string) {
  return useSuspenseQuery(latestServerDrainsQueryOptions(organizationSlug)).data;
}
