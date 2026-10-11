import { queryOptions, useQuery, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { unreachableHost } from "./uptime";
import type { UptimeRange } from "./uptime-status";
import {
  readEnvironmentUptimeServerFn,
  readServiceUptimeServerFn,
  readServiceUptimeSeriesServerFn,
} from "./uptime-status.functions";

export type UptimeScope = { readonly organizationSlug: string; readonly environmentId: string };

/** Every check in the Environment, and when its open incident opened. */
export function environmentUptimeQueryOptions(scope: UptimeScope) {
  return queryOptions({
    queryKey: ["uptime", scope.environmentId, "checks"] as const,
    queryFn: () => readEnvironmentUptimeServerFn({ data: scope }),
    staleTime: 30_000,
    // The checker writes without a Cloud write to invalidate on.
    refetchInterval: 60_000,
  });
}

/** One Service's status, uptime, 90 days and incidents, or null without a check. */
export function serviceUptimeQueryOptions(scope: UptimeScope, serviceId: string) {
  return queryOptions({
    queryKey: ["uptime", scope.environmentId, serviceId] as const,
    queryFn: () => readServiceUptimeServerFn({ data: { ...scope, serviceId } }),
    staleTime: 30_000,
    refetchInterval: 60_000,
  });
}

/** One Service's response time over `range`. */
export function serviceUptimeSeriesQueryOptions(scope: UptimeScope, serviceId: string, range: UptimeRange) {
  return queryOptions({
    queryKey: ["uptime", scope.environmentId, serviceId, range] as const,
    queryFn: () => readServiceUptimeSeriesServerFn({ data: { ...scope, serviceId, range } }),
    staleTime: 30_000,
    refetchInterval: 60_000,
  });
}

/** The hostname a Service's open incident is about, or null. Never suspends: a failed read leaves the card as it was. */
export function useUnreachableHost(scope: UptimeScope, serviceId: string) {
  return useQuery({
    ...environmentUptimeQueryOptions(scope),
    select: (checks) => unreachableHost(checks, serviceId),
  }).data ?? null;
}

export function useServiceUptime(scope: UptimeScope, serviceId: string) {
  return useSuspenseQuery(serviceUptimeQueryOptions(scope, serviceId));
}

export function useServiceUptimeSeries(scope: UptimeScope, serviceId: string, range: UptimeRange) {
  return useSuspenseQuery(serviceUptimeSeriesQueryOptions(scope, serviceId, range)).data;
}

/** Starts the Monitoring tab's reads for `range` before it is shown: on hover of its tab or a range. */
export function usePrefetchServiceUptime(scope: UptimeScope, serviceId: string) {
  const queryClient = useQueryClient();
  return (range: UptimeRange) => {
    void queryClient.prefetchQuery(serviceUptimeQueryOptions(scope, serviceId));
    void queryClient.prefetchQuery(serviceUptimeSeriesQueryOptions(scope, serviceId, range));
  };
}
