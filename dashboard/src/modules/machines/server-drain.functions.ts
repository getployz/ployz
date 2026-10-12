import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { RequestServerDrainInput } from "#/modules/machines/server-drain";
import { listLatestServerDrains, requestServerDrain } from "#/modules/machines/server-drain.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const listLatestServerDrainsServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, listLatestServerDrains(context.actor, data)));

export const requestServerDrainServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RequestServerDrainInput))
  .handler(({ context, data }) => runActor(context, Effect.flatMap(
    dashboardCaller(getRequest().headers, data.organizationSlug),
    (caller) => requestServerDrain(caller, data),
  )));
