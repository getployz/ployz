import { createServerFn } from "@tanstack/react-start";
import { ReadEnvironmentUptimeInput, ReadServiceUptimeInput } from "#/modules/uptime/uptime-status";
import { readEnvironmentUptime, readServiceUptime } from "#/modules/uptime/uptime-status.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const readEnvironmentUptimeServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadEnvironmentUptimeInput))
  .handler(({ context, data }) => runActor(context, readEnvironmentUptime(context.actor, data)));

export const readServiceUptimeServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadServiceUptimeInput))
  .handler(({ context, data }) => runActor(context, readServiceUptime(context.actor, data)));
