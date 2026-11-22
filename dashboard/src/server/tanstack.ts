import { createMiddleware, createServerOnlyFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { strictParseOptions } from "#/lib/schema";
import { Auth } from "#/server/auth.server";
import { encodePublicBoundaryError } from "#/server/public-error";
import { currentRequestScope, RequestScope } from "#/server/request-scope.server";
import { runAppEffect } from "#/server/run.server";
import type { AppServices } from "#/server/runtime.server";

export const publicErrorMiddleware = createMiddleware({ type: "function" })
  .server(async ({ next }) => {
    try {
      return await next();
    } catch (error) {
      throw encodePublicBoundaryError(error);
    }
  });

export const actorMiddleware = createMiddleware({ type: "function" })
  .server(async ({ next }) => {
    const request = getRequest();
    const actor = await runActor(request, Effect.flatMap(Auth, (auth) => auth.resolveActor(request.headers)));
    return next({ context: { actor, signal: request.signal } });
  });

export function strictValidator<S extends Schema.ConstraintDecoder<unknown>>(
  schema: S,
) {
  return Schema.toStandardSchemaV1(schema, { parseOptions: strictParseOptions });
}

/**
 * Runs a server function's `effect` with its Request's `RequestScope`, so the session and access are looked up once per
 * request. A response that outlives its request (SSE, log streams) re-checks with `runAppEffect`, never `runActor`.
 */
export const runActor = createServerOnlyFn(function runActor<A, E>(
  context: { readonly signal: AbortSignal },
  effect: Effect.Effect<A, E, AppServices>,
) {
  return runAppEffect(Effect.provideService(effect, RequestScope, currentRequestScope()), { signal: context.signal });
});
