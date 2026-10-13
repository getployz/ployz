import { useQuery } from "@tanstack/react-query";
import { ItemDescription } from "#/components/ui/item";
import { bytesText, cpuText } from "#/modules/metrics/metric-chart-data";
import { serviceMetrics } from "#/modules/metrics/metrics-model";
import { runtimeServiceMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
import { NOW_RANGE } from "./server-now";
import { serviceUsageOn } from "./server-usage";

/**
 * A Service's CPU and memory on this Server over the last five minutes. Core reads a Service across every Server, so
 * each row costs one read that asks them all; the row keeps only this Server's share.
 */
export function ServiceUsage({ organizationSlug, server, namespace, name }: {
  organizationSlug: string;
  server: { id: string; name: string };
  namespace: string;
  name: string;
}) {
  const { data, isError, dataUpdatedAt } = useQuery(runtimeServiceMetricsQueryOptions({ organizationSlug, namespace, name, ...NOW_RANGE }));
  if (data === undefined && !isError) return null;
  const usage = data === undefined || isError ? null : serviceUsageOn(serviceMetrics(data, new Map([[server.id, server.name]])), server.id, Date.now() - dataUpdatedAt);
  return (
    <ItemDescription className="whitespace-nowrap tabular-nums">
      {usage === null ? "–" : `${cpuText(usage.cpu)} vCPU · ${bytesText(usage.memory)}`}
    </ItemDescription>
  );
}
