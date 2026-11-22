import { createServerFn } from "@tanstack/react-start";
import { ReadRuntimeServiceMetricsInput, ReadServerMetricsInput, ReadServiceMetricsInput } from "#/modules/metrics/metrics";
import { readRuntimeServiceMetrics, readServerMetrics, readServiceMetrics } from "#/modules/metrics/metrics.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";

export const readServiceMetricsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(ReadServiceMetricsInput))
  .handler(({ context, data }) => runActor(context, readServiceMetrics(context.actor, data)));

export const readServerMetricsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(ReadServerMetricsInput))
  .handler(({ context, data }) => runActor(context, readServerMetrics(context.actor, data)));

export const readRuntimeServiceMetricsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(ReadRuntimeServiceMetricsInput))
  .handler(({ context, data }) => runActor(context, readRuntimeServiceMetrics(context.actor, data)));
