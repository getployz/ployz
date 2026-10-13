import { useQuery } from "@tanstack/react-query";
import type { MetricRead } from "#/modules/metrics/metrics";
import { serviceMetrics } from "#/modules/metrics/metrics-model";
import { serviceMarkersQueryOptions } from "#/modules/metrics/metrics.queries";
import { containerTrouble } from "./node-status";

const NO_NAMES: ReadonlyMap<string, string> = new Map();
const trouble = (read: MetricRead) => containerTrouble(serviceMetrics(read, NO_NAMES).markers);

/**
 * An OOM kill or a crash-looping replica of a Service in the last hour, or null. Never suspends: loading, a failed read,
 * or Servers that need an upgrade or didn't answer leave the card as it was.
 */
export function useContainerTrouble(scope: { organizationSlug: string; environmentId: string }, service: string) {
  return useQuery({ ...serviceMarkersQueryOptions(scope, service), select: trouble }).data ?? null;
}
