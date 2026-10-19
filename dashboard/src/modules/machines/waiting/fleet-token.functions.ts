import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import { CreateFleetTokenInput, ListFleetTokensInput, RevokeFleetTokenInput } from "#/modules/machines/waiting/fleet-token";
import { createFleetToken, listFleetTokens, revokeFleetToken } from "#/modules/machines/waiting/fleet-token.server";
import { ployzVersion } from "#/modules/runtime/ployz.server";
import { AppConfig } from "#/server/config.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

export const createFleetTokenServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(CreateFleetTokenInput))
  .handler(({ context, data }) => runActor(context, Effect.gen(function* () {
    const created = yield* createFleetToken(context.actor, data);
    return { ...created, cloudOrigin: (yield* AppConfig).app.url.origin, ployzVersion: ployzVersion() };
  })));

export const listFleetTokensServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(ListFleetTokensInput))
  .handler(({ context, data }) => runActor(context, listFleetTokens(context.actor, data)));

export const revokeFleetTokenServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(RevokeFleetTokenInput))
  .handler(({ context, data }) => runActor(context, revokeFleetToken(context.actor, data)));
