import { Effect, type Schema } from "effect";
import type { Caller } from "#/modules/identity/actor";
import { cliReply } from "#/routes/api/cli/-cli.handler";
import { handleCliRequest } from "#/routes/api/cli/-handlers";
import { AppConfig } from "#/server/config.server";

export function handleSealingRequest<I extends { readonly organizationId: string }, A, E, R>(
  request: Request,
  input: Schema.Decoder<I>,
  operation: (caller: Caller, input: I) => Effect.Effect<A, E, R>,
) {
  return Effect.gen(function* () {
    if (!(yield* AppConfig).sealedVariables) return yield* cliReply(request);
    return yield* handleCliRequest(request, input, operation);
  });
}
