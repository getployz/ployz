import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { serviceVolumes } from "#/modules/config-store/store-lookup.server";
import { queryObserveOrg } from "#/modules/observe-store/clickhouse.server";
import { DISK_METRIC, HOST_METRICS, SCRAPES_METRIC, SERVICE_METRICS, VOLUME_METRICS } from "./metrics-history";
import type { SeriesWindow } from "./metrics-series";

/** Where a read looks: the range, and the bucket width in milliseconds that `stepFor` chose. */
export type StoreRange = { readonly window: SeriesWindow; readonly stepMs: number };

const metricRow = Schema.Struct({
  metric: Schema.String, machine: Schema.String, series: Schema.String, state: Schema.String, direction: Schema.String,
  device: Schema.String, operation: Schema.String, bucket: Schema.Number, sampledAt: Schema.Number, samples: Schema.Number, value: Schema.Number,
});
const diskRow = Schema.Struct({ machine: Schema.String, kind: Schema.String, bucket: Schema.Number, value: Schema.Number });
const crashRow = Schema.Struct({
  at: Schema.Number, machine: Schema.String, container: Schema.String, serviceId: Schema.String, namespace: Schema.String,
  service: Schema.String, reason: Schema.Literals(["out_of_memory", "exit_nonzero"]), exitCode: Schema.NullOr(Schema.Number),
});

const params = (values: Readonly<Record<string, string | number>>) => new Map(Object.entries(values).map(([name, value]) => [name, String(value)]));

const names = (metrics: Readonly<Record<string, string>>) => Object.values(metrics).map((name) => `'${name}'`).join(", ");

/** Seconds since the epoch of the bucket a sample falls in. The same arithmetic as `Math.floor(at / stepMs) * stepMs`. */
const BUCKET = "intDiv(toUnixTimestamp(TimeUnix), {step:UInt32}) * {step:UInt32}";
/** A counter keeps its last value in the bucket, so growth between buckets is exact. A gauge keeps its mean. */
const VALUE = "if(any(IsMonotonic), argMax(Value, TimeUnix), avg(Value))";
/** How many samples the series took in the bucket, which `servicePoints` weighs against the Machine's scrapes. */
const SAMPLES = "toFloat64(count())";
/** When the bucket's last sample was taken, in Unix milliseconds: a counter's growth is over the time between two of these. */
const SAMPLED_AT = "toFloat64(toUnixTimestamp64Milli(max(TimeUnix)))";
/** The identity of one series of a counter: every attribute that fans it out. Two with different identities grow on their own. */
const identity = (...parts: readonly string[]) => `concat(${parts.join(", '|', ")})`;
const IN_RANGE = "TimeUnix >= fromUnixTimestamp64Milli({from:Int64}) AND TimeUnix <= fromUnixTimestamp64Milli({to:Int64})";

/** One bucket earlier than the range, so the first bucket in view has a predecessor to grow from. */
const bounds = ({ window, stepMs }: StoreRange) => ({ from: window.from - stepMs, to: window.to, step: stepMs / 1000 });

const SERVICE_IN = (metrics: string) => `
      AND MetricName IN (${metrics})
      AND ResourceAttributes['ployz.namespace'] = {namespace:String}
      AND ResourceAttributes['ployz.service.name'] = {service:String}
      AND ${IN_RANGE}`;

/**
 * A Service's rows. Beside each container's own, two kinds of row say how to read them. `SCRAPES_METRIC` rows count
 * the distinct scrapes of the Service's memory per Machine and bucket: a replica that lived half a bucket weighs half
 * in it, so a rollout with no overlap never adds the old replica to the new. The Machine's total memory rows tell a
 * container's own limit from the Machine's memory, which Docker reports as the limit of a container with none. A limit
 * of 0, which Docker reports on the last scrape of a stopped container, is left out so it never averages into one.
 */
export const readServiceRows = Effect.fn("MetricsStore.serviceRows")(function* (organizationId: string, range: StoreRange, namespace: string, service: string) {
  const own = { ...bounds(range), namespace, service };
  const [containers, scrapes, machines] = yield* Effect.all([
    queryObserveOrg(organizationId, `
    SELECT MetricName AS metric,
      ResourceAttributes['ployz.machine'] AS machine,
      ${identity("ResourceAttributes['container.id']", "Attributes['interface']", "Attributes['device_major']", "Attributes['device_minor']")} AS series,
      '' AS state, '' AS direction, '' AS device,
      Attributes['operation'] AS operation,
      ${BUCKET} AS bucket,
      ${SAMPLED_AT} AS sampledAt,
      ${SAMPLES} AS samples,
      ${VALUE} AS value
    FROM otel.otel_metrics_sum
    WHERE OrgId = {org:String}${SERVICE_IN(names(SERVICE_METRICS))}
      AND NOT (MetricName = '${SERVICE_METRICS.memoryLimit}' AND Value = 0)
    GROUP BY metric, machine, series, operation, bucket
    ORDER BY bucket`, params(own), metricRow),
    queryObserveOrg(organizationId, `
    SELECT '${SCRAPES_METRIC}' AS metric,
      ResourceAttributes['ployz.machine'] AS machine,
      '' AS series, '' AS state, '' AS direction, '' AS device, '' AS operation,
      ${BUCKET} AS bucket,
      ${SAMPLED_AT} AS sampledAt,
      toFloat64(1) AS samples,
      toFloat64(uniqExact(TimeUnix)) AS value
    FROM otel.otel_metrics_sum
    WHERE OrgId = {org:String}${SERVICE_IN(`'${SERVICE_METRICS.memory}'`)}
    GROUP BY machine, bucket
    ORDER BY bucket`, params(own), metricRow),
    queryObserveOrg(organizationId, `
    SELECT MetricName AS metric,
      ResourceAttributes['ployz.machine'] AS machine,
      '' AS series, '' AS state, '' AS direction, '' AS device, '' AS operation,
      ${BUCKET} AS bucket,
      ${SAMPLED_AT} AS sampledAt,
      ${SAMPLES} AS samples,
      avg(Value) AS value
    FROM otel.otel_metrics_sum
    WHERE OrgId = {org:String}
      AND MetricName = '${HOST_METRICS.memoryTotal}'
      AND ResourceAttributes['ployz.machine'] IN (
        SELECT ResourceAttributes['ployz.machine'] FROM otel.otel_metrics_sum
        WHERE OrgId = {org:String}${SERVICE_IN(`'${SERVICE_METRICS.memory}'`)}
        GROUP BY ResourceAttributes['ployz.machine'])
      AND ${IN_RANGE}
    GROUP BY metric, machine, bucket
    ORDER BY bucket`, params(own), metricRow),
  ]);
  return [...containers, ...scrapes, ...machines];
});

/**
 * The ZFS IO of the managed Volumes named by their Docker Volume names, one series per Volume and Machine. ployzd
 * names a Volume by its Namespace and the rest of its Docker name on each point.
 */
export const readVolumeRows = Effect.fn("MetricsStore.volumeRows")(function* (organizationId: string, range: StoreRange, volumes: readonly string[]) {
  if (volumes.length === 0) return [];
  const listed = volumes.map((volume, index) => [`volume${index}`, volume] as const);
  return yield* queryObserveOrg(organizationId, `
    SELECT MetricName AS metric,
      ResourceAttributes['ployz.machine'] AS machine,
      concat(Attributes['ployz.namespace'], '_', Attributes['ployz.volume']) AS series,
      '' AS state, '' AS direction, '' AS device, '' AS operation,
      ${BUCKET} AS bucket,
      ${SAMPLED_AT} AS sampledAt,
      ${SAMPLES} AS samples,
      argMax(Value, TimeUnix) AS value
    FROM otel.otel_metrics_sum
    WHERE OrgId = {org:String}
      AND MetricName IN (${names(VOLUME_METRICS)})
      AND concat(Attributes['ployz.namespace'], '_', Attributes['ployz.volume']) IN (${listed.map(([name]) => `{${name}:String}`).join(", ")})
      AND ${IN_RANGE}
    GROUP BY metric, machine, series, bucket
    ORDER BY bucket`, params({ ...bounds(range), ...Object.fromEntries(listed) }), metricRow);
});

export const readHostRows = Effect.fn("MetricsStore.hostRows")(function* (organizationId: string, range: StoreRange, machine: string) {
  return yield* queryObserveOrg(organizationId, `
    SELECT MetricName AS metric,
      ResourceAttributes['ployz.machine'] AS machine,
      ${identity("Attributes['cpu']", "Attributes['state']", "Attributes['device']", "Attributes['direction']")} AS series,
      Attributes['state'] AS state,
      Attributes['direction'] AS direction,
      Attributes['device'] AS device,
      '' AS operation,
      ${BUCKET} AS bucket,
      ${SAMPLED_AT} AS sampledAt,
      ${SAMPLES} AS samples,
      ${VALUE} AS value
    FROM otel.otel_metrics_sum
    WHERE OrgId = {org:String}
      AND MetricName IN (${names(HOST_METRICS)})
      AND ResourceAttributes['ployz.machine'] = {machine:String}
      AND ${IN_RANGE}
    GROUP BY metric, machine, series, state, direction, device, bucket
    ORDER BY bucket`, params({ ...bounds(range), machine }), metricRow);
});

export const readDiskRows = Effect.fn("MetricsStore.diskRows")(function* (organizationId: string, range: StoreRange, machine: string) {
  return yield* queryObserveOrg(organizationId, `
    SELECT ResourceAttributes['ployz.machine'] AS machine,
      Attributes['kind'] AS kind,
      ${BUCKET} AS bucket,
      argMax(Value, TimeUnix) AS value
    FROM otel.otel_metrics_gauge
    WHERE OrgId = {org:String}
      AND MetricName = '${DISK_METRIC}'
      AND ResourceAttributes['ployz.machine'] = {machine:String}
      AND ${IN_RANGE}
    GROUP BY machine, kind, bucket
    ORDER BY bucket`, params({ ...bounds(range), machine }), diskRow);
});

/** Whose readings `latestReading` looks for: a Machine's, or a Service's on every Machine. */
export type ReadingScope = { readonly machine: string } | { readonly namespace: string; readonly service: string };

/** When the newest reading since `since` arrived, in Unix milliseconds, or null when none has. */
export const latestReading = Effect.fn("MetricsStore.latestReading")(function* (organizationId: string, scope: ReadingScope, since: number) {
  const [row] = yield* queryObserveOrg(organizationId, `
    SELECT toUInt32(count()) AS readings, ${SAMPLED_AT} AS latest
    FROM otel.otel_metrics_sum
    WHERE OrgId = {org:String}
      AND ${"machine" in scope
        ? "ResourceAttributes['ployz.machine'] = {machine:String}"
        : "ResourceAttributes['ployz.namespace'] = {namespace:String} AND ResourceAttributes['ployz.service.name'] = {service:String}"}
      AND TimeUnix >= fromUnixTimestamp64Milli({since:Int64})`, params({ ...scope, since }), Schema.Struct({ readings: Schema.Number, latest: Schema.Number }));
  return row === undefined || row.readings === 0 ? null : row.latest;
});

/** A Service's rows with its Volumes' on every Machine, and when it last reported. */
export const readServiceReadings = Effect.fn("MetricsStore.serviceReadings")(function* (organizationId: string, range: StoreRange, namespace: string, service: string) {
  const [rows, volumes, latest] = yield* Effect.all([
    readServiceRows(organizationId, range, namespace, service),
    serviceVolumes(organizationId, namespace, service).pipe(Effect.flatMap((names) => readVolumeRows(organizationId, range, names))),
    latestReading(organizationId, { namespace, service }, range.window.from),
  ]);
  return { rows: [...rows, ...volumes], latest };
});

/** A Machine's host and disk rows, and when it last reported, looking back at least a minute before `at`. */
export const readHostReadings = Effect.fn("MetricsStore.hostReadings")(function* (organizationId: string, range: StoreRange, machine: string, at: number) {
  const [rows, disk, latest] = yield* Effect.all([
    readHostRows(organizationId, range, machine),
    readDiskRows(organizationId, range, machine),
    latestReading(organizationId, { machine }, Math.min(range.window.from, at - 60_000)),
  ]);
  return { rows, disk, latest };
});

/**
 * Crash events are log records. Their attributes sit on the record or, when the Collector lifts them, on the resource,
 * so each is read from the record first. The same event can arrive twice, so one row per `ployz.event.id` is kept.
 */
const attribute = (key: string) => `if(mapContains(LogAttributes, '${key}'), LogAttributes['${key}'], ResourceAttributes['${key}'])`;

/**
 * Narrows crash events. With nothing set, every Service in the Organization matches. `namespaces` is the list a caller
 * may read: none means no event matches, and leaving it out means every Namespace.
 */
export type CrashFilter = {
  readonly namespace?: string;
  readonly service?: string;
  readonly machine?: string;
  readonly namespaces?: readonly string[];
};

/** The newest crash events a read returns. A crash loop needs only its latest few, so older ones go first. */
export const CRASH_EVENT_LIMIT = 500;

const crashFilter = (filter: CrashFilter) => {
  const listed = (filter.namespaces ?? []).map((namespace, index) => [`readable${index}`, namespace] as const);
  const clauses = [
    filter.namespace === undefined ? "" : `AND ${attribute("ployz.namespace")} = {namespace:String}`,
    filter.service === undefined ? "" : `AND ${attribute("ployz.service.name")} = {service:String}`,
    filter.machine === undefined ? "" : "AND ResourceAttributes['ployz.machine'] = {machine:String}",
    filter.namespaces === undefined ? "" : listed.length === 0 ? "AND 0" : `AND ${attribute("ployz.namespace")} IN (${listed.map(([name]) => `{${name}:String}`).join(", ")})`,
  ].join("\n      ");
  return {
    clauses,
    values: {
      ...(filter.namespace !== undefined && { namespace: filter.namespace }),
      ...(filter.service !== undefined && { service: filter.service }),
      ...(filter.machine !== undefined && { machine: filter.machine }),
      ...Object.fromEntries(listed),
    },
  };
};

const EVENT_KEY = `if(${attribute("ployz.event.id")} != '', ${attribute("ployz.event.id")}, concat(${attribute("container.id")}, toString(Timestamp)))`;
const CRASH_WHERE = `OrgId = {org:String}
      AND Timestamp >= fromUnixTimestamp64Milli({from:Int64}) AND Timestamp <= fromUnixTimestamp64Milli({to:Int64})
      AND ${attribute("ployz.exit.reason")} IN ('out_of_memory', 'exit_nonzero')`;

/** The newest `CRASH_EVENT_LIMIT` out-of-memory kills and non-zero exits in the range, oldest first. `countCrashEvents` says how many there are. */
export const readCrashEvents = Effect.fn("MetricsStore.crashEvents")(function* (organizationId: string, { window }: StoreRange, filter: CrashFilter = {}) {
  const { clauses, values } = crashFilter(filter);
  const newestFirst = yield* queryObserveOrg(organizationId, `
    SELECT toFloat64(toUnixTimestamp64Milli(Timestamp)) AS at,
      ResourceAttributes['ployz.machine'] AS machine,
      ${attribute("container.id")} AS container,
      ${attribute("ployz.service.id")} AS serviceId,
      ${attribute("ployz.namespace")} AS namespace,
      ${attribute("ployz.service.name")} AS service,
      ${attribute("ployz.exit.reason")} AS reason,
      toInt32OrNull(${attribute("ployz.exit.code")}) AS exitCode
    FROM otel.otel_logs
    WHERE ${CRASH_WHERE}
      ${clauses}
    ORDER BY Timestamp DESC
    LIMIT 1 BY ${EVENT_KEY}
    LIMIT ${CRASH_EVENT_LIMIT}`, params({ from: window.from, to: window.to, ...values }), crashRow);
  return [...newestFirst].reverse();
});

const countRow = Schema.Struct({ reason: Schema.String, events: Schema.Number });

/** How many out-of-memory kills and non-zero exits the range holds, whatever `readCrashEvents` returns of them. */
export const countCrashEvents = Effect.fn("MetricsStore.countCrashEvents")(function* (organizationId: string, { window }: StoreRange, filter: CrashFilter = {}) {
  const { clauses, values } = crashFilter(filter);
  const rows = yield* queryObserveOrg(organizationId, `
    SELECT ${attribute("ployz.exit.reason")} AS reason, toFloat64(uniqExact(${EVENT_KEY})) AS events
    FROM otel.otel_logs
    WHERE ${CRASH_WHERE}
      ${clauses}
    GROUP BY reason`, params({ from: window.from, to: window.to, ...values }), countRow);
  const of = (reason: string) => rows.find((row) => row.reason === reason)?.events ?? 0;
  return { oomKills: of("out_of_memory"), exits: of("exit_nonzero") };
});
