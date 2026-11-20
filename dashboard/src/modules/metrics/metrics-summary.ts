import type { MetricRow } from "@ployz/sdk";
import type { Gap, Sample, ServerMetrics, ServiceMetrics } from "./metrics-model";

/** A newest bucket older than this many steps is history, not the current value. */
const FRESH_STEPS = 3;

type Unit = "cores" | "share" | "bytes" | "bytes_per_second";

/** Decimal units, as storage limits are written. */
export function bytesText(value: number): string {
  const units = ["B", "KB", "MB", "GB", "TB"];
  let scaled = value;
  let index = 0;
  while (scaled >= 1000 && index < units.length - 1) {
    scaled /= 1000;
    index++;
  }
  return index === 0 || scaled >= 10 ? `${scaled.toFixed(0)} ${units[index]}` : `${scaled.toFixed(1)} ${units[index]}`;
}

const show = (unit: Unit, value: number): string => {
  switch (unit) {
    case "cores": return `${value.toFixed(2)} vCPU`;
    case "share": return `${(value * 100).toFixed(0)}%`;
    case "bytes": return bytesText(value);
    case "bytes_per_second": return `${bytesText(value)}/s`;
  }
};

type Line = { readonly label: string; readonly unit: Unit; readonly samples: readonly Sample[]; readonly of?: readonly Sample[] };

/** A metric with no reading has none of now, avg and peak: it is not measured, never zero. */
function row({ label, unit, samples, of }: Line, freshSince: number): MetricRow {
  const last = samples.at(-1);
  if (last === undefined) return { metric: label, now: null, avg: null, peak: null };
  const values = samples.map((sample) => sample.value);
  const limit = last.at >= freshSince ? of?.find((sample) => sample.at === last.at) : undefined;
  const now = last.at < freshSince ? null : limit === undefined ? show(unit, last.value) : `${show(unit, last.value)} / ${show(unit, limit.value)}`;
  return {
    metric: label, now,
    avg: show(unit, values.reduce((sum, value) => sum + value, 0) / values.length),
    peak: show(unit, Math.max(...values)),
  };
}

const freshSince = (until: number, stepMs: number | null) => until - FRESH_STEPS * (stepMs ?? 0);

const rowsOf = (lines: readonly Line[], until: number, stepMs: number | null): MetricRow[] =>
  lines.every((line) => line.samples.length === 0) ? [] : lines.map((line) => row(line, freshSince(until, stepMs)));

/** The Service's table, summed over its Servers. Empty when no Server reported a reading in the range. */
export function serviceRows(metrics: ServiceMetrics, until: number): MetricRow[] {
  const { all } = metrics;
  return rowsOf([
    { label: "CPU", unit: "cores", samples: all.cpu },
    { label: "Memory", unit: "bytes", samples: all.memory, of: all.memoryLimit },
    { label: "Net in", unit: "bytes_per_second", samples: all.netIn },
    { label: "Net out", unit: "bytes_per_second", samples: all.netOut },
    { label: "Disk read", unit: "bytes_per_second", samples: all.diskRead },
    { label: "Disk write", unit: "bytes_per_second", samples: all.diskWrite },
  ], until, metrics.stepMs);
}

/** The Server's table: CPU, memory, each own interface, and its disk by kind. */
export function serverRows(metrics: ServerMetrics, until: number): MetricRow[] {
  const disk = (pick: (sample: ServerMetrics["disk"][number]) => number | undefined): Sample[] =>
    metrics.disk.flatMap((sample) => { const value = pick(sample); return value === undefined ? [] : [{ at: sample.at, value }]; });
  return rowsOf([
    { label: "CPU", unit: "share", samples: metrics.cpu },
    { label: "Memory", unit: "bytes", samples: metrics.memoryUsed, of: metrics.memoryTotal },
    ...metrics.interfaces.flatMap((entry) => [
      { label: `Net ${entry.name} in`, unit: "bytes_per_second", samples: entry.netIn } satisfies Line,
      { label: `Net ${entry.name} out`, unit: "bytes_per_second", samples: entry.netOut } satisfies Line,
    ]),
    { label: "Disk", unit: "bytes", samples: disk((sample) => sample.total - sample.free), of: disk((sample) => sample.total) },
    { label: "Volumes", unit: "bytes", samples: disk((sample) => sample.volumes) },
    { label: "Mirrors", unit: "bytes", samples: disk((sample) => sample.mirrors) },
    { label: "Logs and metrics", unit: "bytes", samples: disk((sample) => sample.logsAndMetrics) },
    { label: "Other", unit: "bytes", samples: disk((sample) => sample.other) },
  ], until, metrics.stepMs);
}

const span = (ms: number): string => {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) return `${seconds}s`;
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h`;
  return `${Math.floor(seconds / 86_400)}d`;
};

/** One note for the time in the range that no reading covers. */
export function gapNotes(gaps: readonly Gap[]): string[] {
  const total = gaps.reduce((sum, gap) => sum + (gap.to - gap.from), 0);
  return total <= 0 ? [] : [`No readings for ${span(total)} of the range.`];
}
