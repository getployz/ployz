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

/** Where a disk's bytes go, ending with what is free. A Server that reports no breakdown shows its used bytes as one. */
export function diskSegments(sample: DiskSample): DiskSegment[] {
  const parts = BREAKDOWN.flatMap(({ key, label }) => {
    const bytes = sample[key];
    return bytes === undefined ? [] : [{ key, label, bytes }];
  });
  const used: DiskSegment[] = parts.length > 0 ? parts : [{ key: "used", label: "Used", bytes: sample.total - sample.free }];
  return [...used, { key: "free", label: "Free", bytes: sample.free }];
}

/** A Service's latest CPU, in vCPUs, and memory on one Server, or null when that Server sent none. */
export function serviceUsageOn(metrics: ServiceMetrics, serverId: string): { readonly cpu: number; readonly memory: number } | null {
  const series = metrics.byServer.find((entry) => entry.server.id === serverId)?.series;
  const cpu = series ? latest(series.cpu) : null;
  const memory = series ? latest(series.memory) : null;
  return cpu === null || memory === null ? null : { cpu, memory };
}
