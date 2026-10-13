import type { DiskSample, ServiceMetrics } from "#/modules/metrics/metrics-model";
import { freshLast } from "./server-now";

export type DiskSegment = { readonly key: "volumes" | "mirrors" | "logsAndMetrics" | "other" | "used" | "free"; readonly label: string; readonly bytes: number };

const BREAKDOWN = [
  { key: "volumes", label: "Volumes" },
  { key: "mirrors", label: "Mirrors" },
  { key: "logsAndMetrics", label: "Logs and metrics" },
  { key: "other", label: "Other" },
] as const satisfies readonly { key: keyof DiskSample; label: string }[];

/**
 * Where a disk's bytes go, ending with what is free. A Server leaves out the parts it didn't measure, so used bytes no
 * part names stay plain Used: they may hold any of the missing parts.
 */
export function diskSegments(sample: DiskSample): DiskSegment[] {
  const parts: DiskSegment[] = BREAKDOWN.flatMap(({ key, label }) => {
    const bytes = sample[key];
    return bytes === undefined ? [] : [{ key, label, bytes }];
  });
  const unnamed = sample.total - sample.free - parts.reduce((sum, part) => sum + part.bytes, 0);
  const used: DiskSegment[] = unnamed > 0 ? [{ key: "used", label: "Used", bytes: unnamed }] : [];
  return [...parts, ...used, { key: "free", label: "Free", bytes: sample.free }];
}

/**
 * A Service's current CPU, in vCPUs, and memory on one Server, from a read that landed `age` ms ago, or null when that
 * Server sent none lately.
 */
export function serviceUsageOn(metrics: ServiceMetrics, serverId: string, age: number): { readonly cpu: number; readonly memory: number } | null {
  const series = metrics.byServer.find((entry) => entry.server.id === serverId)?.series;
  const cpu = series && freshLast(series.cpu, metrics, age);
  const memory = series && freshLast(series.memory, metrics, age);
  return cpu && memory ? { cpu: cpu.value, memory: memory.value } : null;
}
