import { createServerFn } from "@tanstack/react-start";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";
import { OffCommand } from "./off";
import { shutDownPrEnvironment, startPrEnvironment } from "./off.server";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const shutDownPrEnvironmentServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(OffCommand))
  .handler(({ context, data }) => runActor(context, shutDownPrEnvironment(context.actor, data)));

export const startPrEnvironmentServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(OffCommand))
  .handler(({ context, data }) => runActor(context, startPrEnvironment(context.actor, data)));
