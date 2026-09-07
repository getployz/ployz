import { createMiddleware, createServerOnlyFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { Auth } from "#/server/auth.server";
import { encodePublicBoundaryError } from "#/server/public-error";
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
    const actor = await runAppEffect(
      Effect.flatMap(Auth, (auth) => auth.resolveActor(request.headers)),
      { signal: request.signal },
    );
    return next({ context: { actor, signal: request.signal } });
  });

const strictParseOptions = { onExcessProperty: "error" } as const;

export function strictValidator<S extends Schema.ConstraintDecoder<unknown>>(
  schema: S,
) {
  return Schema.toStandardSchemaV1(schema, { parseOptions: strictParseOptions });
}

export const runActor = createServerOnlyFn(function runActor<A, E>(
  context: { readonly signal: AbortSignal },
  effect: Effect.Effect<A, E, AppServices>,
) {
  return runAppEffect(effect, { signal: context.signal });
});
