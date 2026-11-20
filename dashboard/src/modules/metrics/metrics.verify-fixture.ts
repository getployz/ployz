import { DISK_KINDS, HOST_METRICS, SERVICE_METRICS, type CrashEvent, type DiskRow, type MetricRow } from "./metrics-history";

export const NORMAL = "0123456789abcdef0123456789abcdef";
export const ONES = "1".repeat(32);
export const TWOS = "2".repeat(32);
/** Up and sending metrics, with nothing in the range. */
export const FOURS = "4".repeat(32);
const MIB = 1024 * 1024;
const CORES = 16;

type Range = { readonly window: { readonly from: number; readonly to: number }; readonly stepMs: number };

/** The bucket starts of the range, in Unix seconds, as ClickHouse buckets it. */
const bucketsOf = ({ window, stepMs }: Range): number[] => {
  const starts: number[] = [];
  for (let at = Math.floor(window.from / stepMs) * stepMs; at < window.to; at += stepMs) starts.push(at / 1000);
  return starts;
};

const wave = (index: number, base: number, swing: number, seed: number) => base + swing * Math.sin(index / 9 + seed);

/** A counter that grows by `rate(index)` each second, as one cumulative value per bucket. */
const counter = (count: number, stepSec: number, rate: (index: number) => number): number[] => {
  let total = 0;
  return Array.from({ length: count }, (_, index) => (total += rate(index) * stepSec));
};

const row = (metric: string, machine: string, bucket: number, value: number, extra: Partial<MetricRow> = {}): MetricRow =>
  ({ metric, machine, series: "c0", state: "", direction: "", device: "", operation: "", bucket, sampledAt: bucket * 1000, value, ...extra });

/** `skipped` buckets send nothing, so the range shows a gap there. */
const sent = <T>(starts: readonly number[], skipped: (index: number) => boolean, make: (bucket: number, index: number) => readonly T[]) =>
  starts.flatMap((bucket, index) => skipped(index) ? [] : make(bucket, index));

function serviceRows(machine: string, range: Range, seed: number, io: boolean, skipped: (index: number) => boolean = () => false): MetricRow[] {
  const starts = bucketsOf(range);
  const stepSec = range.stepMs / 1000;
  const cpu = counter(starts.length, stepSec, (index) => wave(index, 0.3, 0.2, seed) * 1e9);
  const rx = counter(starts.length, stepSec, (index) => wave(index, 40_000, 30_000, seed));
  const tx = counter(starts.length, stepSec, (index) => wave(index, 25_000, 20_000, seed + 1));
  const read = counter(starts.length, stepSec, (index) => wave(index, 2 * MIB, 1.5 * MIB, seed + 2));
  const write = counter(starts.length, stepSec, (index) => wave(index, MIB, 0.8 * MIB, seed + 3));
  return sent(starts, skipped, (bucket, index) => [
    row(SERVICE_METRICS.cpu, machine, bucket, cpu[index] ?? 0),
    row(SERVICE_METRICS.memory, machine, bucket, wave(index, 200 * MIB, 40 * MIB, seed)),
    row(SERVICE_METRICS.memoryLimit, machine, bucket, 512 * MIB),
    row(SERVICE_METRICS.rx, machine, bucket, rx[index] ?? 0),
    row(SERVICE_METRICS.tx, machine, bucket, tx[index] ?? 0),
    ...(io ? [
      row(SERVICE_METRICS.io, machine, bucket, read[index] ?? 0, { operation: "read" }),
      row(SERVICE_METRICS.io, machine, bucket, write[index] ?? 0, { operation: "write" }),
    ] : []),
  ]);
}

export type ServiceData = { readonly rows: MetricRow[]; readonly crashes: CrashEvent[] };
export type ServerData = { readonly rows: MetricRow[]; readonly disk: DiskRow[]; readonly reporting: boolean };

/** A Service's rows and crash events as ClickHouse would hold them, so the verify dashboard draws what a real read would. */
export function verifyServiceData(service: string, range: Range): ServiceData {
  switch (service) {
    case "web":
      return { rows: [...serviceRows(NORMAL, range, 0, false), ...serviceRows(ONES, range, 1.3, false)], crashes: [] };
    case "api":
      return { rows: serviceRows(NORMAL, range, 0.6, false), crashes: [] };
    case "worker":
      return { rows: serviceRows(NORMAL, range, 2.1, false), crashes: [] };
    case "postgres": {
      const starts = bucketsOf(range);
      const third = Math.floor(starts.length / 3);
      const crashAt = (starts[2 * third + 2] ?? starts.at(-1) ?? 0) * 1000;
      const crash = { machine: NORMAL, container: "c0ffee000000", serviceId: "svc-postgres", namespace: "shop-production", service };
      return {
        rows: serviceRows(NORMAL, range, 2, true, (index) => index >= third && index < 2 * third),
        crashes: [{ ...crash, at: crashAt, reason: "out_of_memory", exitCode: null }, { ...crash, at: crashAt + range.stepMs / 2, reason: "exit_nonzero", exitCode: 1 }],
      };
    }
    default:
      return { rows: [], crashes: [] };
  }
}

/** A Server's rows as ClickHouse would hold them. `reporting` is whether it sent anything in the last minute. */
export function verifyServerData(machine: string, range: Range): ServerData {
  if (machine !== NORMAL) return { rows: [], disk: [], reporting: machine === FOURS };
  const starts = bucketsOf(range);
  const stepSec = range.stepMs / 1000;
  const middle = Math.floor(starts.length / 2);
  const skipped = (index: number) => index >= middle && index < middle + 8;
  const busy = counter(starts.length, stepSec, (index) => wave(index, 0.35, 0.2, 0) * CORES);
  const idle = counter(starts.length, stepSec, (index) => (1 - wave(index, 0.35, 0.2, 0)) * CORES);
  const total = 64 * 1024 * MIB;
  const links = [
    { device: "eth0", receive: (index: number) => wave(index, 900_000, 600_000, 0), transmit: (index: number) => wave(index, 400_000, 300_000, 1) },
    { device: "lo", receive: () => 12_000, transmit: () => 12_000 },
    { device: "docker0", receive: (index: number) => wave(index, 300_000, 100_000, 2), transmit: (index: number) => wave(index, 280_000, 100_000, 3) },
  ].map((link) => ({ ...link, received: counter(starts.length, stepSec, link.receive), transmitted: counter(starts.length, stepSec, link.transmit) }));
  const rows = sent(starts, skipped, (bucket, index) => [
    row(HOST_METRICS.cpu, machine, bucket, busy[index] ?? 0, { state: "user" }),
    row(HOST_METRICS.cpu, machine, bucket, idle[index] ?? 0, { state: "idle" }),
    row(HOST_METRICS.memoryTotal, machine, bucket, total),
    row(HOST_METRICS.memoryAvailable, machine, bucket, total - wave(index, 24 * 1024 * MIB, 4 * 1024 * MIB, 1)),
    ...links.flatMap((link) => [
      row(HOST_METRICS.network, machine, bucket, link.received[index] ?? 0, { device: link.device, direction: "receive" }),
      row(HOST_METRICS.network, machine, bucket, link.transmitted[index] ?? 0, { device: link.device, direction: "transmit" }),
    ]),
  ]);
  const disk = 2_000_000 * MIB;
  const disks = sent(starts, skipped, (bucket, index) => {
    const parts: [string, number][] = [
      [DISK_KINDS.volumes, 500_000 * MIB + index * 100 * MIB], [DISK_KINDS.mirrors, 120_000 * MIB],
      [DISK_KINDS.logsAndMetrics, 30_000 * MIB], [DISK_KINDS.other, 150_000 * MIB],
    ];
    const used = parts.reduce((sum, [, bytes]) => sum + bytes, 0);
    const kinds: [string, number][] = [[DISK_KINDS.total, disk], [DISK_KINDS.free, disk - used], ...parts];
    return kinds.map(([kind, value]): DiskRow => ({ machine, kind, bucket, value }));
  });
  return { rows, disk: disks, reporting: true };
}
