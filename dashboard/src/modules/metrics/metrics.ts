import { Schema } from "effect";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import { RangeEdge } from "#/modules/uptime/uptime-range";

export const ReadServiceMetricsInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  environmentId: Schema.NonEmptyString,
  service: Schema.NonEmptyString,
  from: RangeEdge,
  to: RangeEdge,
  /** One bucket for the whole range, for a caller that reads only the markers. Every Server still answers. */
  markersOnly: Schema.optional(Schema.Boolean),
});
export type ReadServiceMetricsInput = typeof ReadServiceMetricsInput.Type;

/** A Service by the namespace and name the Engine runs it under, for pages that list what runs on a Server. */
export const ReadRuntimeServiceMetricsInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  namespace: Schema.NonEmptyString,
  name: Schema.NonEmptyString,
  from: RangeEdge,
  to: RangeEdge,
});
export type ReadRuntimeServiceMetricsInput = typeof ReadRuntimeServiceMetricsInput.Type;

export const ReadServerMetricsInput = Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  machineId: rustMachineIdSchema,
  from: RangeEdge,
  to: RangeEdge,
});
export type ReadServerMetricsInput = typeof ReadServerMetricsInput.Type;

/** The range a read covers, in Unix milliseconds. */
export type MetricWindow = { readonly from: number; readonly to: number };

/** A span of the range that no reading covers, in Unix milliseconds. */
export type Gap = { readonly from: number; readonly to: number };

/** A Service's readings in one bucket. `at` is the bucket start in Unix milliseconds; a field the bucket lacks is no reading, not a zero. */
export type ServicePoint = {
  readonly at: number;
  /** vCPUs in use. */
  readonly cpu?: number;
  readonly memory?: number;
  readonly memoryLimit?: number;
  readonly netIn?: number;
  readonly netOut?: number;
  readonly diskRead?: number;
  readonly diskWrite?: number;
};

/** A Server's disk in bytes. `total` is the whole disk, so the parts and `free` are the slices of it. */
export type DiskUse = {
  readonly total: number;
  readonly free: number;
  readonly volumes?: number;
  readonly mirrors?: number;
  readonly logsAndMetrics?: number;
  readonly other?: number;
};

/** A Server's readings in one bucket. `cpuBusy` is the share of all CPUs busy, 0 to 1. */
export type HostPoint = {
  readonly at: number;
  readonly cpuBusy?: number;
  readonly memory?: { readonly total: number; readonly available: number };
  readonly interfaces?: Readonly<Record<string, { readonly rx: number; readonly tx: number }>>;
  readonly disk?: DiskUse;
};

export type CrashMarker = { readonly kind: "oom" | "exit"; readonly at: number; readonly machine: string; readonly replica: string; readonly exitCode?: number };

export type ServiceOnServer = { readonly machine: string; readonly points: readonly ServicePoint[]; readonly gaps: readonly Gap[]; readonly markers: readonly CrashMarker[] };

/** A Service's history: `points` sum every Server's, and each Server's own sit beside them. */
export type ServiceHistory = { readonly stepMs: number; readonly points: readonly ServicePoint[]; readonly servers: readonly ServiceOnServer[] };

export type HostHistory = { readonly stepMs: number; readonly points: readonly HostPoint[]; readonly gaps: readonly Gap[] };

/** `at` is when the server read, so a chart anchors on the same instant when it renders and when it hydrates. */
export type MetricRead<History> =
  /** Cloud has no ClickHouse, so it keeps no metrics. */
  | { readonly type: "not_configured"; readonly window: MetricWindow; readonly at: number }
  /** A Server sent nothing for the last minute and nothing in the range. */
  | { readonly type: "not_reporting"; readonly window: MetricWindow; readonly at: number }
  /** `silentSince` is the newest reading when nothing has arrived for a minute, so the history stops there. */
  | { readonly type: "history"; readonly window: MetricWindow; readonly at: number; readonly history: History; readonly silentSince?: number };
