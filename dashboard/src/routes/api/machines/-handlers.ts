import { Effect, Option, Result } from "effect";
import { crashPushInput, decodeCrashPush, decodePageviewPush, decodePushHeader, isObserveStream, pageviewPushInput } from "#/modules/observe-ingest/ingest";
import { authenticateObservePush, ingestCrashPush, ingestPageviewPush, observePushCredentials } from "#/modules/observe-ingest/ingest.server";
import { ackSalts } from "#/modules/web-analytics/salts.server";
import { encodePublicError, NotFound, statusForPublicError, Validation } from "#/server/public-error";

const noStore = { "cache-control": "no-store" };
const MAX_PUSH_BYTES = 256 * 1024;

function errorResponse(cause: unknown) {
  const publicError = encodePublicError(cause);
  return Response.json({ error: publicError.message }, { status: statusForPublicError(publicError), headers: noStore });
}

/** The body's bytes, or null once it says or proves it is over the cap. Stops reading at the cap. */
async function readCappedBody(request: Request) {
  if (Number(request.headers.get("content-length")) > MAX_PUSH_BYTES) return null;
  if (request.body === null) return new Uint8Array(0);
  const reader = request.body.getReader();
  const chunks: Uint8Array[] = [];
  let size = 0;
  for (let read = await reader.read(); !read.done; read = await reader.read()) {
    size += read.value.byteLength;
    if (size > MAX_PUSH_BYTES) {
      await reader.cancel();
      return null;
    }
    chunks.push(read.value);
  }
  return Buffer.concat(chunks);
}

export function handleObservePush(request: Request, machineId: string) {
  return Effect.gen(function* () {
    const credentials = yield* Effect.result(observePushCredentials(machineId, request.headers));
    if (Result.isFailure(credentials)) return errorResponse(credentials.failure);
    const body = yield* Effect.tryPromise(() => readCappedBody(request)).pipe(Effect.orElseSucceed(() => new Uint8Array(0)));
    if (body === null) return Response.json({ error: "Observe push is over 256 KiB." }, { status: 413, headers: noStore });
    const pusher = yield* Effect.result(authenticateObservePush(credentials.success, body));
    if (Result.isFailure(pusher)) return errorResponse(pusher.failure);

    const json: unknown = Option.getOrNull(yield* Effect.option(Effect.try(() => JSON.parse(new TextDecoder().decode(body)))));
    const malformed = () => errorResponse(new Validation({ message: "Invalid observe push." }));
    const header = decodePushHeader(json);
    if (Option.isNone(header)) return malformed();
    if (!isObserveStream(header.value.stream)) {
      return errorResponse(new NotFound({ message: `Unknown observe stream ${header.value.stream}.` }));
    }
    let ingested;
    if (header.value.stream === "pageviews") {
      const push = Option.flatMap(decodePageviewPush(json), pageviewPushInput);
      if (Option.isNone(push)) return malformed();
      ingested = yield* Effect.result(ingestPageviewPush(pusher.success, push.value));
    } else {
      const push = Option.flatMap(decodeCrashPush(json), crashPushInput);
      if (Option.isNone(push)) return malformed();
      ingested = yield* Effect.result(ingestCrashPush(pusher.success, push.value));
    }
    if (Result.isFailure(ingested)) return errorResponse(ingested.failure);
    return yield* ackSalts(pusher.success.organizationId).pipe(
      Effect.map((ack) => Response.json(ack, { headers: noStore })),
      // An empty site list would read as every site turned off, so the Machine keeps its salts instead.
      Effect.catchCause((cause) =>
        Effect.logError("Cannot issue analytics salts; acked the push without them.", { ...pusher.success, cause }).pipe(
          Effect.as(new Response(null, { status: 204, headers: noStore })),
        )),
    );
  });
}
