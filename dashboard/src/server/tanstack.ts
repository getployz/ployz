import { createMiddleware, createServerOnlyFn, type FunctionMiddlewareAfterServer } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { strictParseOptions } from "#/lib/schema";
import { Auth } from "#/modules/identity/auth.server";
import type { Caller } from "#/modules/identity/actor";
import { dashboardCaller } from "#/modules/identity/caller.server";
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

/** Every signed-in server function's middleware, in this order: public errors wrap the Actor lookup. */
export const authenticated = [publicErrorMiddleware, actorMiddleware] as const;

export const OrganizationSlugInput = Schema.Struct({ organizationSlug: Schema.NonEmptyString });

type InputSchema<I, A> = Pick<ReturnType<typeof Schema.toStandardSchemaV1<Schema.Codec<A, I>>>, "~standard">;
type SlugInput = { readonly organizationSlug: string };

/**
 * Validates the whole input, then resolves the dashboard `caller` for its `organizationSlug`, so input errors still come
 * before access errors. Pass it as `.middleware([callerMiddleware(Input)])` and set no validator on the function.
 */
export function callerMiddleware<A extends SlugInput, I>(
  schema: Schema.Codec<A, I>,
): FunctionMiddlewareAfterServer<{}, typeof authenticated, InputSchema<I, A>, { readonly caller: Caller }, undefined, undefined, undefined> {
  // SAFETY: the builder can't resolve a validator type for a generic schema, so it runs with the `SlugInput` view of the
  // same schema (the runtime object is identical) and the result is given the schema's real input and output types.
  const validator = Schema.toStandardSchemaV1(schema as Schema.Codec<SlugInput, SlugInput>, { parseOptions: strictParseOptions });
  const middleware = createMiddleware({ type: "function" })
    .middleware(authenticated)
    .validator(validator)
    .server(async ({ next, data, context }) => {
      const request = getRequest();
      const caller = await runActor(context, Effect.suspend(() => dashboardCaller(request.headers, data.organizationSlug)));
      return next({ context: { caller } });
    });
  // SAFETY: the same middleware object, retyped with the schema's input and output.
  return middleware as never;
}

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
