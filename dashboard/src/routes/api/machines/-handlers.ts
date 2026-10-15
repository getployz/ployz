import { Effect, Option, Result } from "effect";
import { crashPushInput, decodeCrashPush, decodePushHeader, isObserveStream } from "#/modules/observe-ingest/ingest";
import { type AuthenticateObservePush, authenticateObservePush, ingestCrashPush } from "#/modules/observe-ingest/ingest.server";
import { encodePublicError, NotFound, statusForPublicError, Validation } from "#/server/public-error";

const noStore = { "cache-control": "no-store" };

function errorResponse(cause: unknown) {
  const publicError = encodePublicError(cause);
  return Response.json({ error: publicError.message }, { status: statusForPublicError(publicError), headers: noStore });
}

/**
 * Takes a Machine's push of one observe stream. 204 acknowledges it, a replay too. 401 and 404 keep it buffered on the
 * Machine; 422 tells the Machine to drop the batch.
 */
export function handleObservePush<R = never>(
  request: Request,
  machineId: string,
  authenticate: AuthenticateObservePush<R> = authenticateObservePush,
) {
  return Effect.gen(function* () {
    const pusher = yield* Effect.result(authenticate(request, machineId));
    if (Result.isFailure(pusher)) return errorResponse(pusher.failure);

    const json = Option.getOrNull(yield* Effect.option(Effect.tryPromise(() => request.json())));
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
