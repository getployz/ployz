import { useQuery } from "@tanstack/react-query";
import type { MetricRead } from "#/modules/metrics/metrics";
import { readServiceMetricsServerFn } from "#/modules/metrics/metrics.functions";
import { serviceMetrics } from "#/modules/metrics/metrics-model";
import { DEFAULT_RANGE } from "#/modules/uptime/uptime-range";
import { containerTrouble } from "./node-status";

const NO_NAMES: ReadonlyMap<string, string> = new Map();
const trouble = (read: MetricRead) => containerTrouble(serviceMetrics(read, NO_NAMES).markers);

/**
 * An OOM kill or restart of a Service's containers in the last hour, or null. Each read asks every Server in the
 * Cluster, so the card reads one bucket and refreshes slowly. Never suspends: loading, a failed read, or Servers that
 * need an upgrade or didn't answer leave the card as it was.
 */
export function useContainerTrouble(scope: { organizationSlug: string; environmentId: string }, service: string) {
  const input = { ...scope, service, ...DEFAULT_RANGE, markersOnly: true };
  return useQuery({
    queryKey: ["metrics", scope.environmentId, service, "markers", input.from, input.to] as const,
    queryFn: () => readServiceMetricsServerFn({ data: input }),
    select: trouble,
    staleTime: 60_000,
    refetchInterval: 300_000,
  }).data ?? null;
}
