import { Effect, Option, Result, Schema } from "effect";
import { asRecord } from "#/lib/json";
import {
  JoinApprovalRequired,
  ENROLLMENT_PROTOCOL_VERSION,
  enrollmentCallbackBodySchema,
  enrollmentIdentitySchema,
  setupReportSchema,
} from "#/modules/machines/enrollment";
import {
  completeMachineEnrollment,
  enrollMachine,
  recordMachineSetupReport,
} from "#/modules/machines/enrollment.server";
import { bearerToken } from "#/server/bearer";
import { machineErrorResponse, machineRefusal, Unauthorized, Validation } from "#/server/public-error";

const noStore = { "cache-control": "no-store" };

const errorResponse = (cause: unknown) => machineErrorResponse(cause, { codeHeader: true });

/** A failure of the enrollment step itself. The answer names no internal failure, so the log does, with its route. */
function failureResponse(cause: unknown, route: "join" | "completion" | "setup report") {
  return machineErrorResponse(cause, {
    codeHeader: true,
    details: cause instanceof JoinApprovalRequired ? cause.details : undefined,
    report: (kind, logged) => {
      const log = kind === "interruption" ? Effect.logDebug : Effect.logError;
      Effect.runFork(log(`Enrollment ${route} ${kind}.`, logged));
    },
  });
}

const invalidTokenResponse = () => errorResponse(new Unauthorized());

const readJson = (request: Request) =>
  Effect.option(
    Effect.tryPromise({
      try: () => request.json(),
      catch: () => new Validation({ message: "Invalid JSON body." }),
    }),
  );

export const handleMachineEnrollmentJoin = Effect.fn("Enrollment.join")(function* (request: Request) {
  const token = bearerToken(request.headers);
  if (!token) return invalidTokenResponse();

  const body = Option.getOrNull(yield* readJson(request));
  if (asRecord(body)?.["protocolVersion"] !== ENROLLMENT_PROTOCOL_VERSION) {
    return machineRefusal(426, `Unsupported enrollment protocol version; Cloud accepts version ${ENROLLMENT_PROTOCOL_VERSION}.`);
  }
  const parsed = Schema.decodeUnknownOption(enrollmentIdentitySchema)(body);
  if (Option.isNone(parsed)) return errorResponse(new Validation({ message: "Invalid enrollment identity." }));

  const enrolled = yield* Effect.result(enrollMachine({ token, identity: parsed.value }));
  return Result.isFailure(enrolled) ? failureResponse(enrolled.failure, "join") : Response.json(enrolled.success, { headers: noStore });
});

export const handleMachineEnrollmentCallback = Effect.fn("Enrollment.callback")(function* (request: Request) {
  const token = bearerToken(request.headers);
  if (!token) return invalidTokenResponse();

  const parsed = Schema.decodeUnknownOption(enrollmentCallbackBodySchema)(
    Option.getOrNull(yield* readJson(request)),
    { onExcessProperty: "error" },
  );
  if (Option.isNone(parsed)) return errorResponse(new Validation({ message: "Invalid enrollment callback." }));

  const stored = yield* Effect.result(completeMachineEnrollment({ token, ...parsed.value }));
  return Result.isFailure(stored) ? failureResponse(stored.failure, "completion") : Response.json(stored.success, { headers: noStore });
});

export const handleMachineSetupReport = Effect.fn("Enrollment.setupReport")(function* (request: Request) {
  const token = bearerToken(request.headers);
  if (!token) return invalidTokenResponse();

  const parsed = Schema.decodeUnknownOption(setupReportSchema)(Option.getOrNull(yield* readJson(request)));
  if (Option.isNone(parsed)) return errorResponse(new Validation({ message: "Invalid setup report." }));

  const recorded = yield* Effect.result(recordMachineSetupReport({ token, report: parsed.value }));
  return Result.isFailure(recorded)
    ? failureResponse(recorded.failure, "setup report")
    : new Response(null, { status: 204, headers: noStore });
});
