/** @data-source remote: one Service's or one Server's metrics for the shown range, read live from the Servers on each fetch. Stale after 30s and polled every minute because Servers sample without a Cloud write to invalidate on. A canvas card's OOM kills and restarts in the last hour read one bucket per Server, stale after a minute and polled every 5, because every card asks every Server */
import { queryOptions } from "@tanstack/react-query";
import { DEFAULT_RANGE } from "#/lib/time-range";
import type { ReadRuntimeServiceMetricsInput, ReadServerMetricsInput, ReadServiceMetricsInput } from "./metrics";
import { readRuntimeServiceMetricsServerFn, readServerMetricsServerFn, readServiceMetricsServerFn } from "./metrics.functions";

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
export function serverMetricsQueryOptions(input: typeof ReadServerMetricsInput.Encoded) {
  return queryOptions({
    queryKey: ["metrics", "server", input.machineId, input.from, input.to] as const,
    queryFn: () => readServerMetricsServerFn({ data: input }),
    staleTime: 30_000,
    // A Server is not reporting after a minute without a reading, so a page shows it within 90 seconds of the last one.
    refetchInterval: 30_000,
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
    refetchOnWindowFocus: false,
  });
}

/**
 * A runtime Service's metrics over `from`..`to`, by the namespace and name it runs under. Each read asks every Server,
 * so it refreshes when the page mounts or regains focus, never on a timer.
 */
export function runtimeServiceMetricsQueryOptions(input: ReadRuntimeServiceMetricsInput) {
  return queryOptions({
    queryKey: ["metrics", "runtime-service", input.organizationSlug, input.namespace, input.name, input.from, input.to] as const,
    queryFn: () => readRuntimeServiceMetricsServerFn({ data: input }),
    staleTime: 30_000,
  });
}
