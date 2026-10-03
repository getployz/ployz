import { Effect, Option, Result, Schema } from "effect";
import { asRecord } from "#/lib/json";
import {
  encodePublicError,
  statusForPublicError,
  Unauthorized,
  Validation,
} from "#/server/public-error";
import {
  ENROLLMENT_PROTOCOL_VERSION,
  enrollmentCallbackBodySchema,
  enrollmentIdentitySchema,
  setupReportSchema,
  type EnrollResponse,
} from "#/modules/machines/enrollment";
import {
  completeMachineEnrollment,
  enrollMachine,
  recordMachineSetupReport,
} from "#/modules/machines/enrollment.server";

type EnrollmentResponseBody =
  | EnrollResponse
  | { error: string }
  | { machineId: string };

function response(body: EnrollmentResponseBody, status: number) {
  return Response.json(body, {
    status,
    headers: { "cache-control": "no-store" },
  });
}

function errorResponse(cause: unknown) {
  const publicError = encodePublicError(cause);
  return response(
    { error: publicError.message },
    statusForPublicError(publicError),
  );
}

function invalidTokenResponse() {
  return errorResponse(
    new Unauthorized(),
  );
}

type JoinEffect = ReturnType<typeof enrollMachine>;
type JoinRequirements = JoinEffect extends Effect.Effect<
  unknown,
  unknown,
  infer R
>
  ? R
  : never;

type JoinOperation<R> = (
  request: Parameters<typeof enrollMachine>[0],
) => Effect.Effect<Effect.Success<JoinEffect>, Effect.Error<JoinEffect>, R>;

export function handleMachineEnrollmentJoin(
  request: Request,
  token: string,
): Effect.Effect<Response, never, JoinRequirements>;
export function handleMachineEnrollmentJoin<R>(
  request: Request,
  token: string,
  enrollOperation: JoinOperation<R>,
): Effect.Effect<Response, never, R>;
export function handleMachineEnrollmentJoin<R>(
  request: Request,
  token: string,
  enrollOperation?: JoinOperation<R>,
) {
  return Effect.gen(function* () {
    if (!token) return invalidTokenResponse();

    const json = yield* Effect.option(
      Effect.tryPromise({
        try: () => request.json(),
        catch: () => new Validation({ message: "Invalid JSON body." }),
      }),
    );
    const body = Option.getOrNull(json);
    if (asRecord(body)?.["protocolVersion"] !== ENROLLMENT_PROTOCOL_VERSION) {
      return response(
        {
          error: `Unsupported enrollment protocol version; Cloud accepts version ${ENROLLMENT_PROTOCOL_VERSION}.`,
        },
        426,
      );
    }
    const parsed = Schema.decodeUnknownOption(enrollmentIdentitySchema)(body);
    if (Option.isNone(parsed)) {
      return errorResponse(
        new Validation({
          message: "Invalid enrollment identity.",
        }),
      );
    }

    const input = { token, identity: parsed.value };
    const enrolled = enrollOperation
      ? yield* Effect.result(enrollOperation(input))
      : yield* Effect.result(enrollMachine(input));
    if (Result.isFailure(enrolled)) return errorResponse(enrolled.failure);
    return response(enrolled.success, 200);
  });
}

type CompleteEffect = ReturnType<typeof completeMachineEnrollment>;
type CompleteRequirements = CompleteEffect extends Effect.Effect<
  unknown,
  unknown,
  infer R
>
  ? R
  : never;

type CompleteOperation<R> = (
  request: Parameters<typeof completeMachineEnrollment>[0],
) => Effect.Effect<
  Effect.Success<CompleteEffect>,
  Effect.Error<CompleteEffect>,
  R
>;

export function handleMachineEnrollmentCallback(
  request: Request,
  token: string,
): Effect.Effect<Response, never, CompleteRequirements>;
export function handleMachineEnrollmentCallback<R>(
  request: Request,
  token: string,
  completeOperation: CompleteOperation<R>,
): Effect.Effect<Response, never, R>;
export function handleMachineEnrollmentCallback<R>(
  request: Request,
  token: string,
  completeOperation?: CompleteOperation<R>,
) {
  return Effect.gen(function* () {
    if (!token) return invalidTokenResponse();

    const json = yield* Effect.option(
      Effect.tryPromise({
        try: () => request.json(),
        catch: () => new Validation({ message: "Invalid JSON body." }),
      }),
    );
    const parsed = Schema.decodeUnknownOption(enrollmentCallbackBodySchema)(
      Option.getOrNull(json),
      { onExcessProperty: "error" },
    );
    if (Option.isNone(parsed)) {
      return errorResponse(
        new Validation({
          message: "Invalid enrollment callback.",
        }),
      );
    }

    const input = {
      token,
      ...parsed.value,
    };
    const stored = completeOperation
      ? yield* Effect.result(completeOperation(input))
      : yield* Effect.result(completeMachineEnrollment(input));
    if (Result.isFailure(stored)) return errorResponse(stored.failure);
    return response({ machineId: stored.success.machineId }, 200);
  });
}

type RecordReportEffect = ReturnType<typeof recordMachineSetupReport>;
type RecordReportRequirements = RecordReportEffect extends Effect.Effect<unknown, unknown, infer R> ? R : never;

type RecordReportOperation<R> = (
  request: Parameters<typeof recordMachineSetupReport>[0],
) => Effect.Effect<Effect.Success<RecordReportEffect>, Effect.Error<RecordReportEffect>, R>;

export function handleMachineSetupReport(
  request: Request,
  token: string,
): Effect.Effect<Response, never, RecordReportRequirements>;
export function handleMachineSetupReport<R>(
  request: Request,
  token: string,
  recordOperation: RecordReportOperation<R>,
): Effect.Effect<Response, never, R>;
export function handleMachineSetupReport<R>(
  request: Request,
  token: string,
  recordOperation?: RecordReportOperation<R>,
) {
  return Effect.gen(function* () {
    if (!token) return invalidTokenResponse();

    const json = yield* Effect.option(
      Effect.tryPromise({
        try: () => request.json(),
        catch: () => new Validation({ message: "Invalid JSON body." }),
      }),
    );
    const parsed = Schema.decodeUnknownOption(setupReportSchema)(Option.getOrNull(json));
    if (Option.isNone(parsed)) {
      return errorResponse(new Validation({ message: "Invalid setup report." }));
    }

    const input = { token, report: parsed.value };
    const recorded = recordOperation
      ? yield* Effect.result(recordOperation(input))
      : yield* Effect.result(recordMachineSetupReport(input));
    if (Result.isFailure(recorded)) return errorResponse(recorded.failure);
    return new Response(null, { status: 204, headers: { "cache-control": "no-store" } });
  });
}
