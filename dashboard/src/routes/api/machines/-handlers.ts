import { Effect, Option, Result } from "effect";
import { flow } from "effect/Function";
import { recordCrashEventsOf } from "#/modules/observe-ingest/crash-ledger.server";
import { decodePushHeader, decodeRequestPush, isObserveStream, requestPushInput } from "#/modules/observe-ingest/ingest";
import { authenticateObservePush, ingestRequestPush, type ObservePusher, observePushCredentials } from "#/modules/observe-ingest/ingest.server";
import { authenticateMachineToken, forwardOtlp, type OtlpSignal } from "#/modules/observe-ingest/otlp.server";
import type { ObserveStream } from "#/modules/observe-ingest/tables";
import { ackSalts } from "#/modules/web-analytics/salts.server";
import { readCappedBody } from "#/server/capped-body";
import { encodePublicError, NotFound, statusForPublicError, Validation } from "#/server/public-error";

const noStore = { "cache-control": "no-store" };
const MAX_PUSH_BYTES = 256 * 1024;

type IngestRun = ReturnType<typeof ingestRequestPush>;

const ingestStream = {
  requests: (pusher: ObservePusher) =>
    flow(decodeRequestPush, Option.flatMap(requestPushInput), Option.map((push): IngestRun => ingestRequestPush(pusher, push))),
} satisfies Record<ObserveStream, (pusher: ObservePusher) => (json: Parameters<typeof decodeRequestPush>[0]) => Option.Option<IngestRun>>;

function errorResponse(cause: unknown) {
  const publicError = encodePublicError(cause);
  return Response.json({ error: publicError.message }, { status: statusForPublicError(publicError), headers: noStore });
}

const MAX_OTLP_BYTES = 8 * 1024 * 1024;

export function handleMachineOtlp(request: Request, signal: OtlpSignal) {
  return Effect.gen(function* () {
    const machine = yield* Effect.result(authenticateMachineToken(request.headers.get("authorization")));
    if (Result.isFailure(machine)) return errorResponse(machine.failure);
    const read = yield* Effect.result(Effect.tryPromise(() => readCappedBody(request, MAX_OTLP_BYTES)));
    if (Result.isFailure(read)) return Response.json({ error: "Cloud could not read the request body." }, { status: 503, headers: noStore });
    const body = read.success;
    if (body === null) return Response.json({ error: "OTLP body is over 8 MiB." }, { status: 413, headers: noStore });
    const forwarded = yield* Effect.result(forwardOtlp(machine.success.organizationId, signal, request.headers, body));
    if (Result.isFailure(forwarded)) return errorResponse(forwarded.failure);
    if (signal !== "logs" || !forwarded.success.ok) return forwarded.success;
    const recorded = yield* Effect.result(recordCrashEventsOf(machine.success, request.headers, body));
    return Result.isFailure(recorded) ? errorResponse(recorded.failure) : forwarded.success;
  });
}

export function handleObservePush(request: Request, machineId: string) {
  return Effect.gen(function* () {
    const credentials = yield* Effect.result(observePushCredentials(machineId, request.headers));
    if (Result.isFailure(credentials)) return errorResponse(credentials.failure);
    const body = yield* Effect.tryPromise(() => readCappedBody(request, MAX_PUSH_BYTES)).pipe(Effect.orElseSucceed(() => new Uint8Array(0)));
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
    const ingest = ingestStream[header.value.stream](pusher.success)(json);
    if (Option.isNone(ingest)) return malformed();
    const ingested = yield* Effect.result(ingest.value);
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
