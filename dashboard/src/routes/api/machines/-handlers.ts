import { Effect, Result } from "effect";
import { recordCrashEventsOf } from "#/modules/observe-ingest/crash-ledger.server";
import { authenticateObservePush, observePushCredentials, recordSaltFetch } from "#/modules/observe-ingest/ingest.server";
import { authenticateMachineToken, forwardOtlp, type OtlpSignal } from "#/modules/observe-ingest/otlp.server";
import { ackSalts } from "#/modules/web-analytics/salts.server";
import { readCappedBody } from "#/server/capped-body";
import { machineErrorResponse, Validation } from "#/server/public-error";

const noStore = { "cache-control": "no-store" };
const MAX_SALTS_BYTES = 1024;

const MAX_OTLP_BYTES = 8 * 1024 * 1024;

export function handleMachineOtlp(request: Request, signal: OtlpSignal) {
  return Effect.gen(function* () {
    const sender = yield* Effect.result(authenticateMachineToken(request.headers.get("authorization")));
    if (Result.isFailure(sender)) return machineErrorResponse(sender.failure);
    const read = yield* Effect.result(Effect.tryPromise(() => readCappedBody(request, MAX_OTLP_BYTES)));
    if (Result.isFailure(read)) return Response.json({ error: "Cloud could not read the request body." }, { status: 503, headers: noStore });
    const body = read.success;
    if (body === null) return Response.json({ error: "OTLP body is over 8 MiB." }, { status: 413, headers: noStore });
    const forwarded = yield* Effect.result(forwardOtlp(sender.success, signal, request.headers, body));
    if (Result.isFailure(forwarded)) return machineErrorResponse(forwarded.failure);
    if (signal !== "logs" || !forwarded.success.ok) return forwarded.success;
    const recorded = yield* Effect.result(recordCrashEventsOf(sender.success, request.headers, body));
    return Result.isFailure(recorded) ? machineErrorResponse(recorded.failure) : forwarded.success;
  });
}

export function handleAnalyticsSalts(request: Request, machineId: string) {
  return Effect.gen(function* () {
    const credentials = yield* Effect.result(observePushCredentials(machineId, request.headers));
    if (Result.isFailure(credentials)) return machineErrorResponse(credentials.failure);
    const body = yield* Effect.tryPromise(() => readCappedBody(request, MAX_SALTS_BYTES)).pipe(Effect.orElseSucceed(() => new Uint8Array(0)));
    if (body === null) return machineErrorResponse(new Validation({ message: "Analytics salts request is over 1 KiB." }));
    const pusher = yield* Effect.result(authenticateObservePush(credentials.success, body));
    if (Result.isFailure(pusher)) return machineErrorResponse(pusher.failure);
    yield* Effect.result(recordSaltFetch(pusher.success));
    const issued = yield* Effect.result(ackSalts(pusher.success.organizationId));
    // An empty site list would read as every site turned off, so a failure answers an error and the Machine keeps its salts.
    if (Result.isFailure(issued)) {
      yield* Effect.logError("Cannot issue analytics salts.", { ...pusher.success, cause: issued.failure });
      return Response.json({ error: "Cloud could not issue analytics salts." }, { status: 503, headers: noStore });
    }
    return Response.json(issued.success, { headers: noStore });
  });
}
