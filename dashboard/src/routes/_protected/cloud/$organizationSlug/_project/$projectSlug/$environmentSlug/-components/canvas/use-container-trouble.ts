import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import type { MetricRead } from "#/modules/metrics/metrics";
import { type Marker, serviceMetrics } from "#/modules/metrics/metrics-model";
import { serviceMarkersQueryOptions } from "#/modules/metrics/metrics.queries";
import { containerTrouble, LOOP_QUIET_MS, TROUBLE_WINDOW_MS } from "./node-status";

const NO_NAMES: ReadonlyMap<string, string> = new Map();
const NO_MARKERS: readonly Marker[] = [];
const markersOf = (read: MetricRead) => serviceMetrics(read, NO_NAMES).markers;

/**
 * An OOM kill in the last hour or a crash-looping replica of a Service, or null. Never suspends: loading, a failed read,
 * or Servers that need an upgrade or didn't answer leave the card as it was. Expiry runs against the clock, not the
 * read, so a marker expires on time while refetches fail and the last read stays cached.
 */
export function useContainerTrouble(scope: { organizationSlug: string; environmentId: string }, service: string) {
  const { data: markers = NO_MARKERS, dataUpdatedAt } = useQuery({ ...serviceMarkersQueryOptions(scope, service), select: markersOf });
  const [tick, setTick] = useState(Date.now);
  // The timer only wakes for the next expiry, so a read that lands in between brings the clock up to date.
  const now = Math.max(tick, dataUpdatedAt);
  useEffect(() => {
    const expiries = markers.flatMap((marker) => [marker.at + LOOP_QUIET_MS, marker.at + TROUBLE_WINDOW_MS]).filter((expiry) => expiry > now);
    if (expiries.length === 0) return;
    const timer = setTimeout(() => setTick(Date.now()), Math.min(...expiries) - now);
    return () => clearTimeout(timer);
  }, [markers, now]);
  return containerTrouble(markers, now);
}
