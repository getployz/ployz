import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
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
  .handler(({ context, data }) => runActor(context, requestServerDrain(context.actor, data)));
