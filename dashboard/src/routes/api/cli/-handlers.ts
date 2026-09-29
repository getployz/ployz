import { Effect, Schema } from "effect";
import type { Caller } from "#/modules/identity/actor";
import { resolveCaller } from "#/modules/identity/caller.server";
import { Forbidden, Validation } from "#/server/public-error";

/**
 * One CLI call in an Organization: the Caller (a signed-in device or an Organization Token) acts, the JSON body is
 * the input. A token acts only in its own Organization.
 */
export function handleCliRequest<I extends { readonly organizationSlug: string }, A, E, R>(
  request: Request,
  input: Schema.Decoder<I>,
  operation: (caller: Caller, input: I) => Effect.Effect<A, E, R>,
) {
  return Effect.gen(function* () {
    const caller = yield* resolveCaller(request.headers);
    const body = yield* Effect.tryPromise({
      try: () => request.json(),
      catch: () => new Validation({ message: "Invalid JSON body." }),
    });
    const decoded = yield* Schema.decodeUnknownEffect(input)(body, { onExcessProperty: "error" }).pipe(
      Effect.mapError(() => new Validation({ message: "Invalid request." })),
    );
    if (caller.credential.kind === "token" && decoded.organizationSlug !== caller.organization.slug) {
      return yield* new Forbidden({ message: "This Organization Token acts only in its own Organization." });
    }
    return Response.json(yield* operation(caller, decoded), {
      headers: { "cache-control": "no-store" },
    });
  });
}
