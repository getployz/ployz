import { createServerFn } from "@tanstack/react-start";
import { ReadEnvironmentUptimeInput, ReadServiceUptimeInput, RequestUptimeProbeInput } from "#/modules/uptime/uptime-status";
import { readEnvironmentUptime, readServiceUptime, requestUptimeProbe } from "#/modules/uptime/uptime-status.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const readEnvironmentUptimeServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadEnvironmentUptimeInput))
  .handler(({ context, data }) => runActor(context, readEnvironmentUptime(context.actor, data)));

export const readServiceUptimeServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadServiceUptimeInput))
  .handler(({ context, data }) => runActor(context, readServiceUptime(context.actor, data)));

export const requestUptimeProbeServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RequestUptimeProbeInput))
  .handler(({ context, data }) => runActor(context, requestUptimeProbe(context.actor, data)));
