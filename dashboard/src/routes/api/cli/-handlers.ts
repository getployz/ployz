import { Effect, Schema } from "effect";
import { asRecord } from "#/lib/json";
import { runCommand, MANIFEST_HEADER, MANIFEST_REVISION } from "#/modules/commands/commands.server";
import { refusal } from "#/modules/config-store/config-store.server";
import type { Caller } from "#/modules/identity/actor";
import { resolveCaller } from "#/modules/identity/caller.server";
import { REQUEST_KEY_HEADER, requestKeyOf } from "#/modules/operations/keyed.server";
import { Forbidden, publicErrorResponse, Validation } from "#/server/public-error";
import type { AppServices } from "#/server/runtime.server";
import { runAppEffect } from "#/server/run.server";

/** Runs one `/api/cli` call; every reply, a failure too, names the manifest revision. */
export async function serveCli<E>(request: Request, reply: Effect.Effect<Response, E, AppServices>) {
  const response = await runAppEffect(reply, { signal: request.signal }).catch(publicErrorResponse);
  const headers = new Headers(response.headers);
  headers.set(MANIFEST_HEADER, MANIFEST_REVISION);
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

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

/** `POST /api/cli/run/{name}`: one Cloud-published command, its input under `input`. */
export const handleRunRequest = Effect.fn("Commands.handleRun")(function* (request: Request, name: string) {
  const caller = yield* resolveCaller(request.headers);
  const key = requestKeyOf(request.headers.get(REQUEST_KEY_HEADER));
  if (!key.ok) return refusal(key.refusal);
  const body = yield* Effect.tryPromise({
    try: () => request.json(),
    catch: () => new Validation({ message: "Expected a JSON body.", userFacing: true }),
  });
  const input = asRecord(body)?.["input"] ?? null;
  const ran = yield* runCommand(caller, name, input, { approvalId: request.headers.get("x-ployz-approval"), key: key.key });
  if (!ran.ok) return refusal(ran.refusal);
  const reply = ran.message === undefined ? { output: ran.output } : { output: ran.output, message: ran.message };
  return Response.json(reply, { headers: { "cache-control": "no-store" } });
});
