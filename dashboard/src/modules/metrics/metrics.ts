import type { MetricHistory } from "@ployz/sdk";
import { Schema } from "effect";
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

export const ReadServerMetricsInput =Schema.Struct({
  organizationSlug: Schema.NonEmptyString,
  machineId: Schema.String.check(Schema.isPattern(/^[0-9a-f]{32}$/u)),
  from: RangeEdge,
  to: RangeEdge,
});
export type ReadServerMetricsInput = typeof ReadServerMetricsInput.Type;

/** The range a read covers, in Unix milliseconds. */
export type MetricWindow = { readonly from: number; readonly to: number };

export type MetricRead =
  | { readonly type: "offline"; readonly window: MetricWindow }
  | { readonly type: "history"; readonly window: MetricWindow; readonly history: MetricHistory };
