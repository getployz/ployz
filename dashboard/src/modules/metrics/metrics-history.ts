import type { ContainerId, HostPoint, MachineId, MetricGap, MetricHistory, MetricHistoryReply, MetricMarker, ServiceId, ServicePoint } from "@ployz/sdk";
import { counterRate, counterShare, gapsBetween, gaugeSum, type Gap, type SeriesSample, type SeriesWindow } from "./metrics-series";

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

/** One `ployz.disk.bytes` reading of a kind of disk use, in one bucket. */
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
const NS_PER_MS = 1e6;
const NS_PER_USEC = 1000;
/** `system.cpu.time` states that are not work. Idle includes iowait, as the old sampler counted it. */
const IDLE_STATES = new Set(["idle", "iowait"]);

const samples = (rows: readonly MetricRow[], keep: (row: MetricRow) => boolean): SeriesSample[] =>
  rows.filter((row) => keep(row) && Number.isFinite(row.value)).map((row) => ({ series: `${row.machine}|${row.series}`, at: row.bucket * 1000, value: row.value }));

const named = (metric: string) => (row: MetricRow) => row.metric === metric;

const timeline = (maps: readonly ReadonlyMap<number, unknown>[], from: number): number[] =>
  [...new Set(maps.flatMap((map) => [...map.keys()]))].filter((at) => at >= from).sort((left, right) => left - right);

const firstBucket = (window: SeriesWindow, stepMs: number) => Math.floor(window.from / stepMs) * stepMs;

/** The Service's buckets: CPU in microseconds per second, memory in bytes, network and block IO in bytes per second. */
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
      ts: at * NS_PER_MS,
      ...(cpuRate !== undefined && { cpu_usec: cpuRate / NS_PER_USEC }),
      ...(memoryBytes !== undefined && { mem_bytes: Math.round(memoryBytes) }),
      ...(limitBytes !== undefined && { mem_limit: Math.round(limitBytes) }),
      ...(received !== undefined && { rx: received }),
      ...(sent !== undefined && { tx: sent }),
      ...(readRate !== undefined && { io_r: readRate }),
      ...(writeRate !== undefined && { io_w: writeRate }),
    };
  });
}

/** The Machine's buckets: busy share of CPU, memory, bytes per second by interface, and the disk breakdown. */
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
    return {
      ts: at * NS_PER_MS,
      ...(busy !== undefined && { cpu_busy: busy }),
      ...(totalBytes !== undefined && availableBytes !== undefined && { memory: { total: Math.round(totalBytes), available: Math.round(availableBytes) } }),
      ...(Object.keys(rates).length > 0 && { interfaces: rates }),
      ...(breakdown !== undefined && { disk: diskOf(breakdown) }),
    };
  });
}

const diskOf = (breakdown: ReadonlyMap<string, number>): NonNullable<HostPoint["disk"]> => {
  const free = breakdown.get("free") ?? 0;
  const used = [...breakdown.entries()].filter(([kind]) => kind !== "free").reduce((sum, [, bytes]) => sum + bytes, 0);
  const volumes = breakdown.get("volumes");
  const logs = breakdown.get("logs");
  const rest = (breakdown.get("images") ?? 0) + (breakdown.get("ployz") ?? 0) + (breakdown.get("other") ?? 0);
  return {
    total: Math.round(used + free),
    free: Math.round(free),
    ...(volumes !== undefined && { volumes: Math.round(volumes) }),
    ...(logs !== undefined && { observe_store: Math.round(logs) }),
    ...(rest > 0 && { other: Math.round(rest) }),
  };
};

/** ClickHouse holds ids as text. Each one was written by a Machine that sent a well-formed id, so the brand is restored here. */
// SAFETY: `machine` is Cloud's own stamp from the verified token, so it is always a well-formed Machine id.
const machineId = (value: string) => value as MachineId;

const toNanoGap = ({ from, to, reason }: Gap): MetricGap => ({ from: from * NS_PER_MS, to: to * NS_PER_MS, reason });

// SAFETY: the ids are the ones the Machine's own crash event carried. The charts only compare them and never parse them.
const markerOf = (event: CrashEvent): MetricMarker => ({
  machine_id: machineId(event.machine), container_id: event.container as ContainerId, service_id: event.serviceId as ServiceId, service: `${event.namespace}/${event.service}`,
  replica: event.container.slice(0, 12), ts: event.at * NS_PER_MS,
  ...(event.reason === "out_of_memory"
    ? { kind: "oom" as const }
    : { kind: "exit" as const, ...(event.exitCode !== null && { exit_code: event.exitCode }), oom: false, stopped: false }),
});

const bucketsOf = (points: readonly { readonly ts: number }[]) => new Set(points.map((point) => point.ts / NS_PER_MS));

const reply = (points: MetricHistoryReply["points"], stepMs: number, gaps: readonly Gap[], markers: readonly MetricMarker[]): MetricHistoryReply => ({
  points, step_nanos: stepMs * NS_PER_MS, gaps: gaps.map(toNanoGap), ...(markers.length > 0 && { markers: [...markers] }),
});

type Reading = { readonly window: SeriesWindow; readonly stepMs: number; readonly at: number };

/** A Service's rows as the history the charts render: summed over every Machine, with each Machine's own reply. */
export function serviceHistory(rows: readonly MetricRow[], crashes: readonly CrashEvent[], { window, stepMs, at }: Reading): MetricHistory {
  const machines = [...new Set([...rows.map((row) => row.machine), ...crashes.map((event) => event.machine)])].filter((machine) => machine !== "").sort();
  const markers = crashes.map((event) => markerOf(event));
  return {
    step_nanos: stepMs * NS_PER_MS,
    points: { service: servicePoints(rows, window, stepMs) },
    machines: machines.map((machine) => {
      const own = servicePoints(rows.filter((row) => row.machine === machine), window, stepMs);
      const gaps = gapsBetween(bucketsOf(own), window, stepMs, at);
      return { machine_id: machineId(machine), value: reply({ service: own }, stepMs, gaps, markers.filter((marker) => marker.machine_id === machineId(machine))) };
    }),
    unanswered: [],
  };
}

/** A Machine's rows as the history the charts render. */
export function hostHistory(machine: string, rows: readonly MetricRow[], disk: readonly DiskRow[], { window, stepMs, at }: Reading): MetricHistory {
  const points = hostPoints(rows, disk, window, stepMs);
  const gaps = gapsBetween(bucketsOf(points), window, stepMs, at);
  return {
    step_nanos: stepMs * NS_PER_MS,
    points: { machine: points },
    machines: [{ machine_id: machineId(machine), value: reply({ machine: points }, stepMs, gaps, []) }],
    unanswered: [],
  };
}

/** A Service's crash events as markers alone, for the canvas card. Every Machine's markers sit in its own reply. */
export function markersOnlyHistory(crashes: readonly CrashEvent[], { stepMs }: Reading): MetricHistory {
  const machines = [...new Set(crashes.map((event) => event.machine))].filter((machine) => machine !== "").sort();
  return {
    step_nanos: stepMs * NS_PER_MS,
    points: { service: [] },
    machines: machines.map((machine) => ({
      machine_id: machineId(machine),
      value: reply({ service: [] }, stepMs, [], crashes.filter((event) => event.machine === machine).map((event) => markerOf(event))),
    })),
    unanswered: [],
  };
}
