import { useQuery } from "@tanstack/react-query";
import { ItemDescription } from "#/components/ui/item";
import { bytesText, cpuText } from "#/modules/metrics/metric-chart-data";
import { serviceMetrics } from "#/modules/metrics/metrics-model";
import { runtimeServiceMetricsQueryOptions } from "#/modules/metrics/metrics.queries";
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
  const { data } = useQuery(runtimeServiceMetricsQueryOptions({ organizationSlug, namespace, name, from: "5m", to: "now" }));
  const usage = data ? serviceUsageOn(serviceMetrics(data, new Map([[server.id, server.name]])), server.id) : null;
  if (usage === null) return null;
  return <ItemDescription className="whitespace-nowrap tabular-nums">{cpuText(usage.cpu)} vCPU · {bytesText(usage.memory)}</ItemDescription>;
}
