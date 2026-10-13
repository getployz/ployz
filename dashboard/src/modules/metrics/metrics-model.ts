import type { HostPoint, MachineFailure, MetricGap, MetricHistory, MetricHistoryReply, MetricPoints, ServicePoint } from "@ployz/sdk";
import { Schema, type Types } from "effect";
import { requirements } from "#/modules/runtime/daemon-compat";
import type { MetricRead, MetricWindow } from "./metrics";

// The SDK exports `RpcError` as its error class; the payload type is only reachable through a reply.
type RpcError = MetricHistory["unanswered"][number]["error"];

export type ServerRef = { readonly id: string; readonly name: string };
/** `at` is Unix milliseconds. */
export type Sample = { readonly at: number; readonly value: number };
export type Gap = { readonly from: number; readonly to: number; readonly reason: "not_sampled" | "timeout" };
export type Marker = {
  readonly kind: "oom" | "exit" | "restart";
  readonly at: number;
  readonly server: ServerRef;
  readonly replica: string;
  readonly exitCode?: number;
  readonly deploymentId?: string;
};

export type MetricAvailability =
  | { readonly type: "normal" }
  | { readonly type: "mixed"; readonly needsUpgrade: readonly ServerRef[]; readonly unanswered: readonly ServerRef[] }
  | { readonly type: "needs_upgrade"; readonly servers: readonly ServerRef[] }
  | { readonly type: "unanswered"; readonly servers: readonly ServerRef[]; readonly needsUpgrade: readonly ServerRef[] }
  | { readonly type: "no_readings" }
  | { readonly type: "offline" };

export type ServiceSeries = {
  /** vCPUs in use. */
  readonly cpu: readonly Sample[];
  readonly memory: readonly Sample[];
  readonly memoryLimit: readonly Sample[];
  readonly netIn: readonly Sample[];
  readonly netOut: readonly Sample[];
  readonly diskRead: readonly Sample[];
  readonly diskWrite: readonly Sample[];
};

export type ServiceMetrics = {
  readonly availability: MetricAvailability;
  readonly window: MetricWindow;
  readonly stepMs: number | null;
  /** Summed over every answering Server. */
  readonly all: ServiceSeries;
  readonly allGaps: readonly Gap[];
  readonly byServer: readonly { readonly server: ServerRef; readonly series: ServiceSeries; readonly gaps: readonly Gap[] }[];
  readonly markers: readonly Marker[];
};

export type DiskSample = {
  readonly at: number;
  readonly total: number;
  readonly free: number;
  readonly volumes?: number;
  readonly mirrors?: number;
  readonly logsAndMetrics?: number;
  readonly other?: number;
};

export type ServerInterface = { readonly name: string; readonly netIn: readonly Sample[]; readonly netOut: readonly Sample[] };

export type ServerMetrics = {
  readonly availability: MetricAvailability;
  readonly window: MetricWindow;
  readonly stepMs: number | null;
  /** Fraction of all CPUs busy, 0 to 1. */
  readonly cpu: readonly Sample[];
  readonly memoryUsed: readonly Sample[];
  readonly memoryTotal: readonly Sample[];
  readonly interfaces: readonly ServerInterface[];
  readonly disk: readonly DiskSample[];
  readonly gaps: readonly Gap[];
};

type Names = ReadonlyMap<string, string>;

const toMs = (nanos: number) => Math.round(nanos / 1e6);
const serverRef = (id: string, names: Names): ServerRef => ({ id, name: names.get(id) ?? id });
const byName = (left: { readonly name: string }, right: { readonly name: string }) =>
  left.name.localeCompare(right.name, undefined, { numeric: true });

const needsUpgrade: (error: RpcError) => boolean = Schema.is(Schema.Struct({
  code: Schema.Literal("unsupported"),
  details: Schema.Struct({ capability: Schema.Literal(requirements.metrics.capability) }),
}));

function availability(history: MetricHistory, names: Names, hasSample: boolean): MetricAvailability {
  const refs = (failures: readonly MachineFailure<RpcError>[]) => failures.map((failure) => serverRef(failure.machine_id, names));
  const refused = refs(history.unanswered.filter((failure) => needsUpgrade(failure.error)));
  const silent = refs(history.unanswered.filter((failure) => !needsUpgrade(failure.error)));
  if (history.unanswered.length === 0) return hasSample ? { type: "normal" } : { type: "no_readings" };
  if (hasSample) return { type: "mixed", needsUpgrade: refused, unanswered: silent };
  // A Server that answered with nothing only says the Service is not there, so the ones that did not answer explain the empty chart.
  return silent.length > 0 ? { type: "unanswered", servers: silent, needsUpgrade: refused } : { type: "needs_upgrade", servers: refused };
}

const toGap = (gap: MetricGap): Gap => ({ from: toMs(gap.from), to: toMs(gap.to), reason: gap.reason });

function mergeGaps(gaps: readonly Gap[]): Gap[] {
  const merged: Gap[] = [];
  for (const gap of [...gaps].sort((left, right) => left.from - right.from)) {
    const last = merged.at(-1);
    if (last !== undefined && gap.from <= last.to) merged[merged.length - 1] = { from: last.from, to: Math.max(last.to, gap.to), reason: worse(last, gap) };
    else merged.push(gap);
  }
  return merged;
}

const worse = (left: Gap, right: Gap): Gap["reason"] => left.reason === "timeout" || right.reason === "timeout" ? "timeout" : "not_sampled";

/** The spans inside a gap of both merged lists. */
function intersectGaps(left: readonly Gap[], right: readonly Gap[]): Gap[] {
  const both: Gap[] = [];
  let i = 0;
  let j = 0;
  for (;;) {
    const l = left[i];
    const r = right[j];
    if (l === undefined || r === undefined) return both;
    const from = Math.max(l.from, r.from);
    const to = Math.min(l.to, r.to);
    if (from < to) both.push({ from, to, reason: worse(l, r) });
    if (l.to < r.to) i++;
    else j++;
  }
}

const inGap = (at: number, gaps: readonly Gap[]) => gaps.some((gap) => at >= gap.from && at < gap.to);

/** One sample per point that has the field and lies outside every gap; a missing field is no reading, not a zero. */
function series<P extends { readonly ts: number }>(points: readonly P[], gaps: readonly Gap[], read: (point: P) => number | undefined): Sample[] {
  const samples: Sample[] = [];
  for (const point of points) {
    const value = read(point);
    const at = toMs(point.ts);
    if (value !== undefined && !inGap(at, gaps)) samples.push({ at, value });
  }
  return samples;
}

const servicePoints = (points: MetricPoints): readonly ServicePoint[] => "service" in points ? points.service : [];
const hostPoints = (points: MetricPoints): readonly HostPoint[] => "machine" in points ? points.machine : [];

function serviceSeries(points: readonly ServicePoint[], gaps: readonly Gap[]): ServiceSeries {
  return {
    cpu: series(points, gaps, (point) => point.cpu_usec === undefined ? undefined : point.cpu_usec / 1e6),
    memory: series(points, gaps, (point) => point.mem_bytes),
    memoryLimit: series(points, gaps, (point) => point.mem_limit),
    netIn: series(points, gaps, (point) => point.rx),
    netOut: series(points, gaps, (point) => point.tx),
    diskRead: series(points, gaps, (point) => point.io_r),
    diskWrite: series(points, gaps, (point) => point.io_w),
  };
}

const hasSamples = (groups: readonly (readonly unknown[])[]) => groups.some((group) => group.length > 0);
const stepMs = (history: MetricHistory) => history.step_nanos === undefined ? null : toMs(history.step_nanos);

function markers(replies: readonly MetricHistoryReply[], names: Names): Marker[] {
  return replies.flatMap((reply) => (reply.markers ?? []).map((marker) => {
    const mapped: Types.Mutable<Marker> = { kind: marker.kind, at: toMs(marker.ts), server: serverRef(marker.machine_id, names), replica: marker.replica };
    if (marker.kind === "exit" && marker.exit_code !== undefined) mapped.exitCode = marker.exit_code;
    if (marker.deployment_id !== undefined) mapped.deploymentId = marker.deployment_id;
    return mapped;
  })).sort((left, right) => left.at - right.at);
}

const EMPTY_SERVICE: ServiceSeries = { cpu: [], memory: [], memoryLimit: [], netIn: [], netOut: [], diskRead: [], diskWrite: [] };

/** A Service's read as charts: the sum over its Servers, each Server's own, and what happened to its containers. */
export function serviceMetrics(read: MetricRead, names: Names): ServiceMetrics {
  if (read.type === "offline") {
    return { availability: { type: "offline" }, window: read.window, stepMs: null, all: EMPTY_SERVICE, allGaps: [], byServer: [], markers: [] };
  }
  const { history } = read;
  const replies = history.machines.map((machine) => ({
    server: serverRef(machine.machine_id, names),
    reply: machine.value,
    gaps: mergeGaps(machine.value.gaps.map(toGap)),
  }));
  // Every up Server answers, and one without the Service answers a not_sampled gap over the whole range, so those gap
  // the sum only where no Server has data. A timeout is a Server whose share is missing, so it gaps the sum wherever it is.
  const sampled = replies.map((entry) => entry.gaps.filter((gap) => gap.reason === "not_sampled"));
  const timedOut = replies.flatMap((entry) => entry.gaps.filter((gap) => gap.reason === "timeout"));
  const allGaps = mergeGaps([...(sampled.length === 0 ? [] : sampled.reduce(intersectGaps)), ...timedOut]);
  const all = serviceSeries(servicePoints(history.points), allGaps);
  const byServer = replies
    .map(({ server, reply, gaps }) => ({ server, series: serviceSeries(servicePoints(reply.points), gaps), gaps, markers: reply.markers ?? [] }))
    .filter((entry) => entry.markers.length > 0 || hasSamples(Object.values(entry.series)))
    .map(({ server, series: own, gaps }) => ({ server, series: own, gaps }))
    .sort((left, right) => byName(left.server, right.server));
  return {
    availability: availability(history, names, hasSamples(Object.values(all))),
    window: read.window,
    stepMs: stepMs(history),
    all,
    allGaps,
    byServer,
    markers: markers(history.machines.map((machine) => machine.value), names),
  };
}

const HIDDEN_INTERFACE = /^(?:lo|veth.*|docker.*|br-.*)$/u;

/** A Server's read as charts: its CPU, memory, network interfaces and disk. */
export function serverMetrics(read: MetricRead, names: Names): ServerMetrics {
  if (read.type === "offline") {
    return {
      availability: { type: "offline" }, window: read.window, stepMs: null,
      cpu: [], memoryUsed: [], memoryTotal: [], interfaces: [], disk: [], gaps: [],
    };
  }
  const { history } = read;
  const gaps = mergeGaps((history.machines[0]?.value.gaps ?? []).map(toGap));
  const points = hostPoints(history.points);
  const interfaceNames = [...new Set(points.flatMap((point) => Object.keys(point.interfaces ?? {})))]
    .filter((name) => !HIDDEN_INTERFACE.test(name));
  const interfaces = interfaceNames.map((name): ServerInterface => ({
    name,
    netIn: series(points, gaps, (point) => point.interfaces?.[name]?.rx),
    netOut: series(points, gaps, (point) => point.interfaces?.[name]?.tx),
  })).sort(byName);
  const disk = points.flatMap((point): DiskSample[] => {
    const at = toMs(point.ts);
    if (point.disk === undefined || inGap(at, gaps)) return [];
    const { total, free, volumes, mirror_slots, observe_store, other } = point.disk;
    const sample: Types.Mutable<DiskSample> = { at, total, free };
    if (volumes !== undefined) sample.volumes = volumes;
    if (mirror_slots !== undefined) sample.mirrors = mirror_slots;
    if (observe_store !== undefined) sample.logsAndMetrics = observe_store;
    if (other !== undefined) sample.other = other;
    return [sample];
  });
  const cpu = series(points, gaps, (point) => point.cpu_busy);
  const memoryUsed = series(points, gaps, (point) => point.memory === undefined ? undefined : point.memory.total - point.memory.available);
  const memoryTotal = series(points, gaps, (point) => point.memory?.total);
  const hasSample = hasSamples([cpu, memoryUsed, memoryTotal, disk, ...interfaces.flatMap((iface) => [iface.netIn, iface.netOut])]);
  return {
    availability: availability(history, names, hasSample),
    window: read.window,
    stepMs: stepMs(history),
    cpu, memoryUsed, memoryTotal, interfaces, disk, gaps,
  };
}
