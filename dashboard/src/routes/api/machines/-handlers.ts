import { Effect, Option, Result } from "effect";
import { crashPushInput, decodeCrashPush, decodePushHeader, isObserveStream } from "#/modules/observe-ingest/ingest";
import { authenticateObservePush, ingestCrashPush } from "#/modules/observe-ingest/ingest.server";
import { encodePublicError, NotFound, statusForPublicError, Validation } from "#/server/public-error";

const noStore = { "cache-control": "no-store" };

function errorResponse(cause: unknown) {
  const publicError = encodePublicError(cause);
  return Response.json({ error: publicError.message }, { status: statusForPublicError(publicError), headers: noStore });
}

/**
 * Takes a Machine's signed push of one observe stream. 204 acknowledges it, a replay too. 401 and 404 keep it buffered on the
 * Machine; 422 tells the Machine to drop the batch.
 */
export function handleObservePush(request: Request, machineId: string) {
  return Effect.gen(function* () {
    const body = new Uint8Array(yield* Effect.tryPromise(() => request.arrayBuffer()).pipe(Effect.orElseSucceed(() => new ArrayBuffer(0))));
    const pusher = yield* Effect.result(authenticateObservePush({ machineId, headers: request.headers, body }));
    if (Result.isFailure(pusher)) return errorResponse(pusher.failure);

    const json: unknown = Option.getOrNull(yield* Effect.option(Effect.try(() => JSON.parse(new TextDecoder().decode(body)))));
    const malformed = () => errorResponse(new Validation({ message: "Invalid observe push." }));
    const header = decodePushHeader(json);
    if (Option.isNone(header)) return malformed();
    if (!isObserveStream(header.value.stream)) {
      return errorResponse(new NotFound({ message: `Unknown observe stream ${header.value.stream}.` }));
    }
    const push = Option.flatMap(decodeCrashPush(json), crashPushInput);
    if (Option.isNone(push)) return malformed();
    const ingested = yield* Effect.result(ingestCrashPush(pusher.success, push.value));
    if (Result.isFailure(ingested)) return errorResponse(ingested.failure);
    return new Response(null, { status: 204, headers: noStore });
  });
}
