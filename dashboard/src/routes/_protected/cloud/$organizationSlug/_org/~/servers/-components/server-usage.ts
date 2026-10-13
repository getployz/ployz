import type { DiskSample, Sample, ServerMetrics, ServiceMetrics } from "#/modules/metrics/metrics-model";

export type Usage = { readonly used: number; readonly total: number };

/** What a Server uses now: its latest reading of each, or null where it has none. */
export type ServerUsage = { readonly cpu: number | null; readonly memory: Usage | null; readonly disk: Usage | null };

const latest = (samples: readonly Sample[]) => samples.at(-1)?.value ?? null;

export function serverUsage(metrics: ServerMetrics): ServerUsage {
  const used = latest(metrics.memoryUsed);
  const total = latest(metrics.memoryTotal);
  const disk = metrics.disk.at(-1);
  return {
    cpu: latest(metrics.cpu),
    memory: used === null || total === null || total <= 0 ? null : { used, total },
    disk: disk === undefined || disk.total <= 0 ? null : { used: disk.total - disk.free, total: disk.total },
  };
}

export type DiskSegment = { readonly key: "volumes" | "mirrors" | "logsAndMetrics" | "other" | "used" | "free"; readonly label: string; readonly bytes: number };

const BREAKDOWN = [
  { key: "volumes", label: "Volumes" },
  { key: "mirrors", label: "Mirrors" },
  { key: "logsAndMetrics", label: "Logs and metrics" },
  { key: "other", label: "Other" },
] as const satisfies readonly { key: keyof DiskSample; label: string }[];

/**
 * Where a disk's bytes go, ending with what is free. A Server leaves out the parts it didn't measure, so used bytes no
 * part names go to Other, or to one Used when the Server named none.
 */
export function diskSegments(sample: DiskSample): DiskSegment[] {
  const parts: DiskSegment[] = BREAKDOWN.flatMap(({ key, label }) => {
    const bytes = sample[key];
    return bytes === undefined ? [] : [{ key, label, bytes }];
  });
  const unnamed = sample.total - sample.free - parts.reduce((sum, part) => sum + part.bytes, 0);
  const rest: DiskSegment | null = unnamed <= 0 ? null
    : parts.length === 0 ? { key: "used", label: "Used", bytes: unnamed }
    : { key: "other", label: "Other", bytes: unnamed + (parts.find((part) => part.key === "other")?.bytes ?? 0) };
  const used = rest === null ? parts : [...parts.filter((part) => part.key !== "other"), rest];
  return [...used, { key: "free", label: "Free", bytes: sample.free }];
}

/** A Service's latest CPU, in vCPUs, and memory on one Server, or null when that Server sent none. */
export function serviceUsageOn(metrics: ServiceMetrics, serverId: string): { readonly cpu: number; readonly memory: number } | null {
  const series = metrics.byServer.find((entry) => entry.server.id === serverId)?.series;
  const cpu = series ? latest(series.cpu) : null;
  const memory = series ? latest(series.memory) : null;
  return cpu === null || memory === null ? null : { cpu, memory };
}
