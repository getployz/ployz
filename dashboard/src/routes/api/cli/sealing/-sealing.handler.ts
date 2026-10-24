import { Effect, Option, Schema } from "effect";
import type { Caller } from "#/modules/identity/actor";
import { resolveCaller } from "#/modules/identity/caller.server";
import { readSealedVariables } from "#/modules/sealing/sealed-variables.server";
import { cliReply } from "#/routes/api/cli/-cli.handler";
import { handleCliRequestAs } from "#/routes/api/cli/-handlers";

const decodeOrganization = Schema.decodeUnknownOption(Schema.fromJsonString(Schema.Struct({ organizationId: Schema.String })));

/** A sealing call answers as the CLI catch-all does unless the Organization its body names has `sealedVariables` on. */
export function handleSealingRequest<I extends { readonly organizationId: string }, A, E, R>(
  request: Request,
  input: Schema.Decoder<I>,
  operation: (caller: Caller, input: I) => Effect.Effect<A, E, R>,
) {
  return Effect.gen(function* () {
    // The catch-all refuses a stranger before it reads a body, so this does too.
    const caller = yield* resolveCaller(request.headers);
    const named = decodeOrganization(yield* Effect.promise(() => request.clone().text().catch(() => "")));
    if (Option.isNone(named) || !(yield* readSealedVariables(named.value.organizationId))) return yield* cliReply(request);
    return yield* handleCliRequestAs(caller, request, input, operation);
  });
}
