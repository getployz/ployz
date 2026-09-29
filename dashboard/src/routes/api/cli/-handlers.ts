import { Effect, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { Auth } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { NotFound, Validation } from "#/server/public-error";

/**
 * One signed-in CLI call: the device's bearer names the Actor, the JSON body is the input.
 * Dark until the Config Store cutover: production answers 404, as it has no bearer sign-in either.
 */
export function handleCliRequest<I, A, E, R>(
  request: Request,
  input: Schema.Decoder<I>,
  operation: (actor: Actor, input: I) => Effect.Effect<A, E, R>,
) {
  return Effect.gen(function* () {
    if ((yield* AppConfig).nodeEnv === "production") {
      return yield* new NotFound({ message: "Not found." });
    }
    const actor = yield* (yield* Auth).resolveActor(request.headers);
    const body = yield* Effect.tryPromise({
      try: () => request.json(),
      catch: () => new Validation({ message: "Invalid JSON body." }),
    });
    const decoded = yield* Schema.decodeUnknownEffect(input)(body, { onExcessProperty: "error" }).pipe(
      Effect.mapError(() => new Validation({ message: "Invalid request." })),
    );
    return Response.json(yield* operation(actor, decoded), {
      headers: { "cache-control": "no-store" },
    });
  });
}
