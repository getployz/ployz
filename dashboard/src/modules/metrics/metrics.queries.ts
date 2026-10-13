import { queryOptions } from "@tanstack/react-query";
import { DEFAULT_RANGE } from "#/modules/uptime/uptime-range";
import type { ReadServerMetricsInput, ReadServiceMetricsInput } from "./metrics";
import { readServerMetricsServerFn, readServiceMetricsServerFn } from "./metrics.functions";

/** One Service's metrics over `from`..`to`, summed across its Servers. */
export function serviceMetricsQueryOptions(input: ReadServiceMetricsInput) {
  return queryOptions({
    queryKey: ["metrics", input.environmentId, input.service, input.from, input.to] as const,
    queryFn: () => readServiceMetricsServerFn({ data: input }),
    staleTime: 30_000,
    // Servers sample without a Cloud write to invalidate on.
    refetchInterval: 60_000,
  });
}

/** One Server's host metrics over `from`..`to`. */
export function serverMetricsQueryOptions(input: ReadServerMetricsInput) {
  return queryOptions({
    queryKey: ["metrics", "server", input.machineId, input.from, input.to] as const,
    queryFn: () => readServerMetricsServerFn({ data: input }),
    staleTime: 30_000,
    refetchInterval: 60_000,
  });
}

/**
 * A Service's OOM kills, exits and restarts in the last hour, for its canvas card. Each read asks every Server in the
 * Cluster, so every Server answers one bucket and the card refetches slowly.
 */
export function serviceMarkersQueryOptions(scope: { organizationSlug: string; environmentId: string }, service: string) {
  const input = { ...scope, service, ...DEFAULT_RANGE, markersOnly: true };
  return queryOptions({
    queryKey: ["metrics", scope.environmentId, service, "markers", input.from, input.to] as const,
    queryFn: () => readServiceMetricsServerFn({ data: input }),
    staleTime: 60_000,
    refetchInterval: 300_000,
  });
}
