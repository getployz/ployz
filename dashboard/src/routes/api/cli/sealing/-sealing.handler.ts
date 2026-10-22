import { Effect, type Schema } from "effect";
import type { Caller } from "#/modules/identity/actor";
import { resolveCaller } from "#/modules/identity/caller.server";
import { handleCliRequest } from "#/routes/api/cli/-handlers";
import { AppConfig } from "#/server/config.server";
import { NotFound } from "#/server/public-error";

/** A Sealing Key recovery call, which without sealed variables answers as any unknown `/api/cli` path. */
export function handleSealingRequest<I extends { readonly organizationId: string }, A, E, R>(
  request: Request,
  input: Schema.Decoder<I>,
  operation: (caller: Caller, input: I) => Effect.Effect<A, E, R>,
) {
  return Effect.gen(function* () {
    if (!(yield* AppConfig).sealedVariables) {
      yield* resolveCaller(request.headers);
      return yield* new NotFound({ message: "Not found." });
    }
    return yield* handleCliRequest(request, input, operation);
  });
}
