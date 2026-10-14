import { createServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { RequestServerWebTrafficInput } from "#/modules/machines/server-web-traffic";
import { listLatestServerWebTraffic, requestServerWebTraffic } from "#/modules/machines/server-web-traffic.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const listLatestServerWebTrafficServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Schema.NonEmptyString })))
  .handler(({ context, data }) => runActor(context, listLatestServerWebTraffic(context.actor, data)));

export const requestServerWebTrafficServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RequestServerWebTrafficInput))
  .handler(({ context, data }) => runActor(context, requestServerWebTraffic(context.actor, data)));
