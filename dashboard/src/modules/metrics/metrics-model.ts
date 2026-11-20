import type { DiskUse, Gap, HostHistory, HostPoint, MetricRead, MetricWindow, ServiceHistory, ServicePoint } from "./metrics";

/** `name` is null for a Server the list doesn't name. */
export type ServerRef = { readonly id: string; readonly name: string | null };
/** `at` is Unix milliseconds. */
export type Sample = { readonly at: number; readonly value: number };
export type { Gap };
export type Marker = {
  readonly kind: "oom" | "exit";
  readonly at: number;
  readonly server: ServerRef;
  readonly replica: string;
  readonly exitCode?: number;
};

export type MetricAvailability =
  | { readonly type: "normal" }
  | { readonly type: "no_readings" }
  /** Cloud keeps no metrics: ClickHouse is not set up. */
  | { readonly type: "not_configured" }
  /** A Server that is up sent nothing for the last minute. */
  | { readonly type: "not_reporting" };

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

export type DiskSample = DiskUse & { readonly at: number };

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

const serverRef = (id: string, names: Names): ServerRef => ({ id, name: names.get(id) ?? null });
const byName = (left: { readonly name: string | null }, right: { readonly name: string | null }) =>
  (left.name ?? "").localeCompare(right.name ?? "", undefined, { numeric: true });

const availability = (hasSample: boolean): MetricAvailability => hasSample ? { type: "normal" } : { type: "no_readings" };

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
    if (from < to) both.push({ from, to });
    if (l.to < r.to) i++;
    else j++;
  }
}

const inGap = (at: number, gaps: readonly Gap[]) => gaps.some((gap) => at >= gap.from && at < gap.to);

/** One sample per point that has the field and lies outside every gap; a missing field is no reading, not a zero. */
function series<P extends { readonly at: number }>(points: readonly P[], gaps: readonly Gap[], read: (point: P) => number | undefined): Sample[] {
  const samples: Sample[] = [];
  for (const point of points) {
    const value = read(point);
    if (value !== undefined && !inGap(point.at, gaps)) samples.push({ at: point.at, value });
  }
  return samples;
}

function serviceSeries(points: readonly ServicePoint[], gaps: readonly Gap[]): ServiceSeries {
  return {
    cpu: series(points, gaps, (point) => point.cpu),
    memory: series(points, gaps, (point) => point.memory),
    memoryLimit: series(points, gaps, (point) => point.memoryLimit),
    netIn: series(points, gaps, (point) => point.netIn),
    netOut: series(points, gaps, (point) => point.netOut),
    diskRead: series(points, gaps, (point) => point.diskRead),
    diskWrite: series(points, gaps, (point) => point.diskWrite),
  };
}

const hasSamples = (groups: readonly (readonly unknown[])[]) => groups.some((group) => group.length > 0);

const EMPTY_SERVICE: ServiceSeries = { cpu: [], memory: [], memoryLimit: [], netIn: [], netOut: [], diskRead: [], diskWrite: [] };

/** A Service's read as charts: the sum over its Servers, each Server's own, and what happened to its containers. */
export function serviceMetrics(read: MetricRead<ServiceHistory>, names: Names): ServiceMetrics {
  if (read.type !== "history") {
    return { availability: { type: read.type }, window: read.window, stepMs: null, all: EMPTY_SERVICE, allGaps: [], byServer: [], markers: [] };
  }
  const { history } = read;
  // A span is a gap in the sum only where no Server has data.
  const allGaps = history.servers.length === 0 ? [] : history.servers.map((entry) => entry.gaps).reduce(intersectGaps);
  const all = serviceSeries(history.points, allGaps);
  const byServer = history.servers
    .map((entry) => ({ server: serverRef(entry.machine, names), series: serviceSeries(entry.points, entry.gaps), gaps: entry.gaps, markers: entry.markers }))
    .filter((entry) => entry.markers.length > 0 || hasSamples(Object.values(entry.series)))
    .map(({ server, series: own, gaps }) => ({ server, series: own, gaps }))
    .sort((left, right) => byName(left.server, right.server));
  const markers = history.servers.flatMap((entry) => entry.markers.map((marker): Marker => ({
    kind: marker.kind, at: marker.at, server: serverRef(marker.machine, names), replica: marker.replica,
    ...(marker.exitCode !== undefined && { exitCode: marker.exitCode }),
  }))).sort((left, right) => left.at - right.at);
  return { availability: availability(hasSamples(Object.values(all))), window: read.window, stepMs: history.stepMs, all, allGaps, byServer, markers };
}

const HIDDEN_INTERFACE = /^(?:lo|veth.*|docker.*|br-.*)$/u;

/** A Server's read as charts: its CPU, memory, network interfaces and disk. */
export function serverMetrics(read: MetricRead<HostHistory>): ServerMetrics {
  if (read.type !== "history") {
    return {
      availability: { type: read.type }, window: read.window, stepMs: null,
      cpu: [], memoryUsed: [], memoryTotal: [], interfaces: [], disk: [], gaps: [],
    };
  }
  const { history } = read;
  const { points, gaps } = history;
  const interfaceNames = [...new Set(points.flatMap((point) => Object.keys(point.interfaces ?? {})))]
    .filter((name) => !HIDDEN_INTERFACE.test(name));
  const interfaces = interfaceNames.map((name): ServerInterface => ({
    name,
    netIn: series(points, gaps, (point) => point.interfaces?.[name]?.rx),
    netOut: series(points, gaps, (point) => point.interfaces?.[name]?.tx),
  })).sort(byName);
  const disk = points.flatMap((point: HostPoint): DiskSample[] => point.disk === undefined || inGap(point.at, gaps) ? [] : [{ at: point.at, ...point.disk }]);
  const cpu = series(points, gaps, (point) => point.cpuBusy);
  const memoryUsed = series(points, gaps, (point) => point.memory === undefined ? undefined : point.memory.total - point.memory.available);
  const memoryTotal = series(points, gaps, (point) => point.memory?.total);
  const hasSample = hasSamples([cpu, memoryUsed, memoryTotal, disk, ...interfaces.flatMap((iface) => [iface.netIn, iface.netOut])]);
  return { availability: availability(hasSample), window: read.window, stepMs: history.stepMs, cpu, memoryUsed, memoryTotal, interfaces, disk, gaps };
}
