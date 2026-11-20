import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { OrgQuery } from "#/modules/observe-store/clickhouse.server";
import { DISK_METRIC, HOST_METRICS, SERVICE_METRICS, type CrashEvent, type DiskRow, type MetricRow } from "./metrics-history";
import type { SeriesWindow } from "./metrics-series";

/** Where a read looks: the range, and the bucket width in milliseconds that `stepFor` chose. */
export type StoreRange = { readonly window: SeriesWindow; readonly stepMs: number };

const names = (metrics: Readonly<Record<string, string>>) => Object.values(metrics).map((name) => `'${name}'`).join(", ");

/** Seconds since the epoch of the bucket a sample falls in. The same arithmetic as `Math.floor(at / stepMs) * stepMs`. */
const BUCKET = "intDiv(toUnixTimestamp(TimeUnix), {step:UInt32}) * {step:UInt32}";
/** A counter keeps its last value in the bucket, so growth between buckets is exact. A gauge keeps its mean. */
const VALUE = "if(any(IsMonotonic), argMax(Value, TimeUnix), avg(Value))";
const IN_RANGE = "TimeUnix >= fromUnixTimestamp64Milli({from:Int64}) AND TimeUnix <= fromUnixTimestamp64Milli({to:Int64})";

/** One bucket earlier than the range, so the first bucket in view has a predecessor to grow from. */
const bounds = ({ window, stepMs }: StoreRange) => ({ from: window.from - stepMs, to: window.to, step: stepMs / 1000 });

/** A Service's container series, on every Machine that runs it. */
export const readServiceRows = Effect.fn("MetricsStore.serviceRows")(function* (query: OrgQuery, range: StoreRange, namespace: string, service: string) {
  return yield* query<MetricRow>(`
    SELECT MetricName AS metric,
      ResourceAttributes['ployz.machine'] AS machine,
      ResourceAttributes['container.id'] AS series,
      '' AS state, '' AS direction, '' AS device,
      Attributes['operation'] AS operation,
      ${BUCKET} AS bucket,
      ${VALUE} AS value
    FROM otel.otel_metrics_sum
    WHERE OrgId = {org:String}
      AND MetricName IN (${names(SERVICE_METRICS)})
      AND ResourceAttributes['ployz.namespace'] = {namespace:String}
      AND ResourceAttributes['ployz.service.name'] = {service:String}
      AND ${IN_RANGE}
    GROUP BY metric, machine, series, operation, bucket
    ORDER BY bucket`, { ...bounds(range), namespace, service });
});

/** A Machine's own series: CPU time by state, memory, and network bytes by interface and direction. */
export const readHostRows = Effect.fn("MetricsStore.hostRows")(function* (query: OrgQuery, range: StoreRange, machine: string) {
  return yield* query<MetricRow>(`
    SELECT MetricName AS metric,
      ResourceAttributes['ployz.machine'] AS machine,
      '' AS series,
      Attributes['state'] AS state,
      Attributes['direction'] AS direction,
      Attributes['device'] AS device,
      '' AS operation,
      ${BUCKET} AS bucket,
      ${VALUE} AS value
    FROM otel.otel_metrics_sum
    WHERE OrgId = {org:String}
      AND MetricName IN (${names(HOST_METRICS)})
      AND ResourceAttributes['ployz.machine'] = {machine:String}
      AND ${IN_RANGE}
    GROUP BY metric, machine, state, direction, device, bucket
    ORDER BY bucket`, { ...bounds(range), machine });
});

/** A Machine's disk use by kind, as `ployz.disk.bytes` reports it. */
export const readDiskRows = Effect.fn("MetricsStore.diskRows")(function* (query: OrgQuery, range: StoreRange, machine: string) {
  return yield* query<DiskRow>(`
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
    ORDER BY bucket`, { ...bounds(range), machine });
});

const RECENT_SECONDS = 60;

/** Whether the Machine sent any metric in the last minute. */
export const machineIsReporting = Effect.fn("MetricsStore.machineIsReporting")(function* (query: OrgQuery, machine: string) {
  const [row] = yield* query<{ readonly recent: number }>(`
    SELECT toUInt32(count()) AS recent
    FROM otel.otel_metrics_sum
    WHERE OrgId = {org:String}
      AND ResourceAttributes['ployz.machine'] = {machine:String}
      AND TimeUnix >= now() - INTERVAL ${RECENT_SECONDS} SECOND`, { machine });
  return (row?.recent ?? 0) > 0;
});

/**
 * Crash events are log records. Their attributes sit on the record or, when the Collector lifts them, on the resource,
 * so each is read from the record first. The same event can arrive twice, so one row per `ployz.event.id` is kept.
 */
const attribute = (key: string) => `if(mapContains(LogAttributes, '${key}'), LogAttributes['${key}'], ResourceAttributes['${key}'])`;

/** Narrows crash events to one Service. With neither field set, every Service in the Organization matches. */
export type CrashFilter = { readonly namespace?: string; readonly service?: string };

/** Out-of-memory kills and non-zero exits of a Service's containers in the range, newest last. */
export const readCrashEvents = Effect.fn("MetricsStore.crashEvents")(function* (query: OrgQuery, { window }: StoreRange, filter: CrashFilter = {}) {
  const namespace = filter.namespace === undefined ? "" : `AND ${attribute("ployz.namespace")} = {namespace:String}`;
  const service = filter.service === undefined ? "" : `AND ${attribute("ployz.service.name")} = {service:String}`;
  return yield* query<CrashEvent>(`
    SELECT toFloat64(toUnixTimestamp64Milli(Timestamp)) AS at,
      ResourceAttributes['ployz.machine'] AS machine,
      ${attribute("container.id")} AS container,
      ${attribute("ployz.service.id")} AS serviceId,
      ${attribute("ployz.namespace")} AS namespace,
      ${attribute("ployz.service.name")} AS service,
      ${attribute("ployz.exit.reason")} AS reason,
      toInt32OrNull(${attribute("ployz.exit.code")}) AS exitCode
    FROM otel.otel_logs
    WHERE OrgId = {org:String}
      AND Timestamp >= fromUnixTimestamp64Milli({from:Int64}) AND Timestamp <= fromUnixTimestamp64Milli({to:Int64})
      AND ${attribute("ployz.exit.reason")} IN ('out_of_memory', 'exit_nonzero')
      ${namespace} ${service}
    ORDER BY Timestamp
    LIMIT 1 BY if(${attribute("ployz.event.id")} != '', ${attribute("ployz.event.id")}, concat(container, toString(Timestamp)))
    LIMIT 500`, {
    from: window.from, to: window.to,
    ...(filter.namespace !== undefined && { namespace: filter.namespace }),
    ...(filter.service !== undefined && { service: filter.service }),
  });
});
