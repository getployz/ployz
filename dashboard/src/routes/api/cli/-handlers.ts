import { Effect, Schema } from "effect";
import { asRecord } from "#/lib/json";
import { runCommand } from "#/modules/commands/commands.server";
import type { Caller } from "#/modules/identity/actor";
import { resolveCaller } from "#/modules/identity/caller.server";
import { REQUEST_KEY_HEADER, requestKeyOf } from "#/modules/operations/keyed.server";
import { cliRefusalResponse, Forbidden, Validation } from "#/server/public-error";

/**
 * One CLI call in an Organization, named by its ID, which a slug rename never moves: the Caller (a signed-in device or
 * an Organization Token) acts, the JSON body is the input. A token acts only in its own Organization.
 */
export function handleOrganizationCliRequest<I extends { readonly organizationId: string }, A, E, R>(
  request: Request,
  input: Schema.Decoder<I>,
  operation: (caller: Caller, input: I) => Effect.Effect<A, E, R>,
) {
  return Effect.flatMap(resolveCaller(request.headers), (caller) => handleOrganizationCliRequestAs(caller, request, input, operation));
}

/** {@link handleOrganizationCliRequest} for a Caller already resolved from this request. */
function handleOrganizationCliRequestAs<I extends { readonly organizationId: string }, A, E, R>(
  caller: Caller,
  request: Request,
  input: Schema.Decoder<I>,
  operation: (caller: Caller, input: I) => Effect.Effect<A, E, R>,
) {
  return Effect.gen(function* () {
    const body = yield* Effect.tryPromise({
      try: () => request.json(),
      catch: () => new Validation({ message: "Invalid JSON body." }),
    });
    const decoded = yield* Schema.decodeUnknownEffect(input)(body, { onExcessProperty: "error" }).pipe(
      Effect.mapError(() => new Validation({ message: "Invalid request." })),
    );
    if (caller.credential.kind === "token" && decoded.organizationId !== caller.organization.id) {
      return yield* new Forbidden({ message: "This Organization Token acts only in its own Organization." });
    }
    return Response.json(yield* operation(caller, decoded), {
      headers: { "cache-control": "no-store" },
    });
  });
}

/** `POST /api/cli/run/{name}`: one Cloud-published command, its input under `input`. */
export const handleRunRequest = Effect.fn("Commands.handleRun")(function* (request: Request, name: string) {
  const caller = yield* resolveCaller(request.headers);
  const key = requestKeyOf(request.headers.get(REQUEST_KEY_HEADER));
  if (!key.ok) return cliRefusalResponse(key.refusal);
  const body = yield* Effect.tryPromise({
    try: () => request.json(),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  const input = asRecord(body)?.["input"] ?? null;
  const ran = yield* runCommand(caller, name, input, { approvalId: request.headers.get("x-ployz-approval"), key: key.key });
  if (!ran.ok) return cliRefusalResponse(ran.refusal);
  const reply = ran.message === undefined ? { output: ran.output } : { output: ran.output, message: ran.message };
  return Response.json(reply, { headers: { "cache-control": "no-store" } });
});
