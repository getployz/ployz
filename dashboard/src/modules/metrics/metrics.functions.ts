import { createServerFn } from "@tanstack/react-start";
import { ReadServerMetricsInput, ReadServiceMetricsInput } from "#/modules/metrics/metrics";
import { readServerMetrics, readServiceMetrics } from "#/modules/metrics/metrics.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const readServiceMetricsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadServiceMetricsInput))
  .handler(({ context, data }) => runActor(context, readServiceMetrics(context.actor, data)));

export const readServerMetricsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadServerMetricsInput))
  .handler(({ context, data }) => runActor(context, readServerMetrics(context.actor, data)));
