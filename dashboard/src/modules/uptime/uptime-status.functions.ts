import { createServerFn } from "@tanstack/react-start";
import { ReadOpenUptimeIncidentsInput, ReadUptimeCheckInput } from "#/modules/uptime/uptime-status";
import { readOpenUptimeIncidents, readUptimeCheck } from "#/modules/uptime/uptime-status.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const readOpenUptimeIncidentsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadOpenUptimeIncidentsInput))
  .handler(({ context, data }) => runActor(context, readOpenUptimeIncidents(context.actor, data)));

export const readUptimeCheckServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ReadUptimeCheckInput))
  .handler(({ context, data }) => runActor(context, readUptimeCheck(context.actor, data)));
