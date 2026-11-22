/** @data-source remote: one Service's or one Server's metrics for the shown range, read from Cloud's ClickHouse, which the Servers' pushes fill. Stale after 30s and polled every minute because a push brings no change to invalidate on. A canvas card's OOM kills and restarts in the last hour are stale after a minute and polled every 5, because every card on the canvas reads them */
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
    // Readings arrive in the Servers' pushes, which bring no change to invalidate on.
    refetchInterval: 60_000,
  });
}

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
 * A Service's OOM kills, exits and restarts in the last hour, for its canvas card. Every card on the canvas reads its
 * own, so the card refetches slowly.
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

/** A runtime Service's metrics over `from`..`to`, by the namespace and name it runs under. */
export function runtimeServiceMetricsQueryOptions(input: ReadRuntimeServiceMetricsInput) {
  return queryOptions({
    queryKey: ["metrics", "runtime-service", input.organizationSlug, input.namespace, input.name, input.from, input.to] as const,
    queryFn: () => readRuntimeServiceMetricsServerFn({ data: input }),
    staleTime: 30_000,
  });
}
