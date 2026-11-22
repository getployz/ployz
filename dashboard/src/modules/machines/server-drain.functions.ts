import { createServerFn } from "@tanstack/react-start";
import { RequestServerDrainInput } from "#/modules/machines/server-drain";
import { listLatestServerDrains, requestServerDrain } from "#/modules/machines/server-drain.server";
import { authenticated, callerMiddleware, OrganizationSlugInput, runActor, strictValidator } from "#/server/tanstack";

export const listLatestServerDrainsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .validator(strictValidator(OrganizationSlugInput))
  .handler(({ context, data }) => runActor(context, listLatestServerDrains(context.actor, data)));

export const requestServerDrainServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(RequestServerDrainInput)])
  .handler(({ context, data }) => runActor(context, requestServerDrain(context.caller, data)));
