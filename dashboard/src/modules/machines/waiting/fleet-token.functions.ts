import { createServerFn } from "@tanstack/react-start";
import { CreateFleetTokenInput, ListFleetTokensInput, RevokeFleetTokenInput } from "#/modules/machines/waiting/fleet-token";
import { createFleetToken, listFleetTokens, revokeFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const createFleetTokenServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(CreateFleetTokenInput))
  .handler(({ context, data }) => runActor(context, createFleetToken(context.actor, data)));

export const listFleetTokensServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ListFleetTokensInput))
  .handler(({ context, data }) => runActor(context, listFleetTokens(context.actor, data)));

export const revokeFleetTokenServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RevokeFleetTokenInput))
  .handler(({ context, data }) => runActor(context, revokeFleetToken(context.actor, data)));
