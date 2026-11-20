import type { CrashMarker, DiskUse, HostHistory, HostPoint, ServiceHistory, ServicePoint } from "./metrics";
import { counterRate, counterShare, gapsBetween, gaugeSum, type SeriesSample, type SeriesWindow } from "./metrics-series";

/** One series in one bucket, as ClickHouse returns it. `bucket` is the bucket start in Unix seconds. */
export type MetricRow = {
  readonly metric: string;
  readonly machine: string;
  readonly series: string;
  readonly state: string;
  readonly direction: string;
  readonly device: string;
  readonly operation: string;
  readonly bucket: number;
  readonly value: number;
};

/** One `ployz.disk.bytes` reading of a part of the disk, in one bucket. `kind` is the part's name in `DISK_KINDS`. */
export type DiskRow = { readonly machine: string; readonly kind: string; readonly bucket: number; readonly value: number };

/** A crash event with the machine it happened on. `at` is Unix milliseconds. */
export type CrashEvent = {
  readonly at: number;
  readonly machine: string;
  readonly container: string;
  readonly serviceId: string;
  readonly namespace: string;
  readonly service: string;
  readonly reason: "out_of_memory" | "exit_nonzero";
  readonly exitCode: number | null;
};

export const SERVICE_METRICS = {
  cpu: "container.cpu.usage.total",
  memory: "container.memory.usage.total",
  memoryLimit: "container.memory.usage.limit",
  rx: "container.network.io.usage.rx_bytes",
  tx: "container.network.io.usage.tx_bytes",
  io: "container.blockio.io_service_bytes_recursive",
} as const;

export const HOST_METRICS = {
  cpu: "system.cpu.time",
  memoryTotal: "system.memory.limit",
  memoryAvailable: "system.linux.memory.available",
  network: "system.network.io",
} as const;

export const DISK_METRIC = "ployz.disk.bytes";

/** The `kind` attribute of `ployz.disk.bytes`: the whole disk, what is free, and the parts of what is used. */
export const DISK_KINDS = { total: "total", free: "free", volumes: "volumes", mirrors: "mirror_slots", logsAndMetrics: "observe_store", other: "other" } as const;

const NS_PER_SECOND = 1e9;
/** The `system.cpu.time` states, as hostmetrics names them, in which a core waits rather than works. Every other state is busy. */
const IDLE_STATES = new Set(["idle", "wait"]);

const samples = (rows: readonly MetricRow[], keep: (row: MetricRow) => boolean): SeriesSample[] =>
  rows.filter((row) => keep(row) && Number.isFinite(row.value)).map((row) => ({ series: `${row.machine}|${row.series}`, at: row.bucket * 1000, value: row.value }));

const named = (metric: string) => (row: MetricRow) => row.metric === metric;

const timeline = (maps: readonly ReadonlyMap<number, unknown>[], from: number): number[] =>
  [...new Set(maps.flatMap((map) => [...map.keys()]))].filter((at) => at >= from).sort((left, right) => left - right);

const firstBucket = (window: SeriesWindow, stepMs: number) => Math.floor(window.from / stepMs) * stepMs;

/** The Service's buckets: CPU in vCPUs, memory in bytes, network and block IO in bytes per second. */
export function servicePoints(rows: readonly MetricRow[], window: SeriesWindow, stepMs: number): ServicePoint[] {
  const cpu = counterRate(samples(rows, named(SERVICE_METRICS.cpu)));
  const memory = gaugeSum(samples(rows, named(SERVICE_METRICS.memory)));
  const limit = gaugeSum(samples(rows, named(SERVICE_METRICS.memoryLimit)));
  const rx = counterRate(samples(rows, named(SERVICE_METRICS.rx)));
  const tx = counterRate(samples(rows, named(SERVICE_METRICS.tx)));
  const read = counterRate(samples(rows, (row) => row.metric === SERVICE_METRICS.io && row.operation === "read"));
  const write = counterRate(samples(rows, (row) => row.metric === SERVICE_METRICS.io && row.operation === "write"));
  return timeline([cpu, memory, limit, rx, tx, read, write], firstBucket(window, stepMs)).map((at): ServicePoint => {
    const [cpuRate, memoryBytes, limitBytes] = [cpu.get(at), memory.get(at), limit.get(at)];
    const [received, sent, readRate, writeRate] = [rx.get(at), tx.get(at), read.get(at), write.get(at)];
    return {
      at,
      ...(cpuRate !== undefined && { cpu: cpuRate / NS_PER_SECOND }),
      ...(memoryBytes !== undefined && { memory: Math.round(memoryBytes) }),
      ...(limitBytes !== undefined && { memoryLimit: Math.round(limitBytes) }),
      ...(received !== undefined && { netIn: received }),
      ...(sent !== undefined && { netOut: sent }),
      ...(readRate !== undefined && { diskRead: readRate }),
      ...(writeRate !== undefined && { diskWrite: writeRate }),
    };
  });
}

/** The disk of one bucket. Without `total` and `free` there is no disk to draw. */
function diskOf(kinds: ReadonlyMap<string, number>): DiskUse | undefined {
  const total = kinds.get(DISK_KINDS.total);
  const free = kinds.get(DISK_KINDS.free);
  if (total === undefined || free === undefined) return undefined;
  const part = (kind: string) => {
    const bytes = kinds.get(kind);
    return bytes === undefined ? undefined : Math.round(bytes);
  };
  const [volumes, mirrors, logsAndMetrics, other] = [part(DISK_KINDS.volumes), part(DISK_KINDS.mirrors), part(DISK_KINDS.logsAndMetrics), part(DISK_KINDS.other)];
  return {
    total: Math.round(total),
    free: Math.round(free),
    ...(volumes !== undefined && { volumes }),
    ...(mirrors !== undefined && { mirrors }),
    ...(logsAndMetrics !== undefined && { logsAndMetrics }),
    ...(other !== undefined && { other }),
  };
}

/** The Machine's buckets: busy share of CPU, memory, bytes per second by interface, and the disk. */
export function hostPoints(rows: readonly MetricRow[], disk: readonly DiskRow[], window: SeriesWindow, stepMs: number): HostPoint[] {
  const cpu = counterShare(
    samples(rows, (row) => row.metric === HOST_METRICS.cpu && !IDLE_STATES.has(row.state)),
    samples(rows, (row) => row.metric === HOST_METRICS.cpu && IDLE_STATES.has(row.state)),
  );
  const total = gaugeSum(samples(rows, named(HOST_METRICS.memoryTotal)));
  const available = gaugeSum(samples(rows, named(HOST_METRICS.memoryAvailable)));
  const devices = [...new Set(rows.filter(named(HOST_METRICS.network)).map((row) => row.device))].filter((device) => device !== "");
  const interfaces = devices.map((device) => {
    const direction = (names: readonly string[]) => counterRate(samples(rows, (row) =>
      row.metric === HOST_METRICS.network && row.device === device && names.includes(row.direction)));
    return { device, rx: direction(["receive"]), tx: direction(["transmit"]) };
  });
  const kinds = new Map<number, Map<string, number>>();
  for (const row of disk) {
    if (!Number.isFinite(row.value)) continue;
    const at = row.bucket * 1000;
    kinds.set(at, (kinds.get(at) ?? new Map()).set(row.kind, row.value));
  }
  return timeline([cpu, total, available, kinds, ...interfaces.flatMap((entry) => [entry.rx, entry.tx])], firstBucket(window, stepMs)).map((at): HostPoint => {
    const [busy, totalBytes, availableBytes] = [cpu.get(at), total.get(at), available.get(at)];
    const rates = Object.fromEntries(interfaces.flatMap((entry) => {
      const received = entry.rx.get(at);
      const sent = entry.tx.get(at);
      return received === undefined && sent === undefined ? [] : [[entry.device, { rx: received ?? 0, tx: sent ?? 0 }] as const];
    }));
    const breakdown = kinds.get(at);
    const diskUse = breakdown === undefined ? undefined : diskOf(breakdown);
    return {
      at,
      ...(busy !== undefined && { cpuBusy: busy }),
      ...(totalBytes !== undefined && availableBytes !== undefined && { memory: { total: Math.round(totalBytes), available: Math.round(availableBytes) } }),
      ...(Object.keys(rates).length > 0 && { interfaces: rates }),
      ...(diskUse !== undefined && { disk: diskUse }),
    };
  });
}

const markerOf = (event: CrashEvent): CrashMarker => ({
  at: event.at, machine: event.machine, replica: event.container.slice(0, 12),
  ...(event.reason === "out_of_memory" ? { kind: "oom" as const } : { kind: "exit" as const, ...(event.exitCode !== null && { exitCode: event.exitCode }) }),
});

const bucketsOf = (points: readonly { readonly at: number }[]) => new Set(points.map((point) => point.at));

type Reading = { readonly window: SeriesWindow; readonly stepMs: number; readonly at: number };

const machinesOf = (rows: readonly { readonly machine: string }[], crashes: readonly CrashEvent[]) =>
  [...new Set([...rows.map((row) => row.machine), ...crashes.map((event) => event.machine)])].filter((machine) => machine !== "").sort();

export function serviceHistory(rows: readonly MetricRow[], crashes: readonly CrashEvent[], { window, stepMs, at }: Reading): ServiceHistory {
  return {
    stepMs,
    points: servicePoints(rows, window, stepMs),
    servers: machinesOf(rows, crashes).map((machine) => {
      const points = servicePoints(rows.filter((row) => row.machine === machine), window, stepMs);
      return {
        machine, points, gaps: gapsBetween(bucketsOf(points), window, stepMs, at),
        markers: crashes.filter((event) => event.machine === machine).map(markerOf),
      };
    }),
  };
}

export function hostHistory(rows: readonly MetricRow[], disk: readonly DiskRow[], { window, stepMs, at }: Reading): HostHistory {
  const points = hostPoints(rows, disk, window, stepMs);
  return { stepMs, points, gaps: gapsBetween(bucketsOf(points), window, stepMs, at) };
}

/** A Service's crash events as markers alone, for the canvas card. */
export function markersOnlyHistory(crashes: readonly CrashEvent[], { stepMs }: Reading): ServiceHistory {
  return {
    stepMs,
    points: [],
    servers: machinesOf([], crashes).map((machine) => ({ machine, points: [], gaps: [], markers: crashes.filter((event) => event.machine === machine).map(markerOf) })),
  };
}
