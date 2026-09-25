import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { buildOrderEditSchema } from "./build-order";
import { setBuildOrder } from "./build-order.server";

export const setBuildOrderServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(buildOrderEditSchema))
  .handler(({ context, data }) => runActor(context, setBuildOrder(context.actor, data)));
