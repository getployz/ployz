import { Cause, Data, Effect, Option, Schema } from "effect";
import {
  PublicError,
  type PublicError as PublicErrorData,
} from "#/lib/public-error";

export class NotFound extends Data.TaggedError("NotFound")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "not-found" as const;
}

export class Conflict extends Data.TaggedError("Conflict")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "conflict" as const;
}

export class Validation extends Data.TaggedError("Validation")<{
  readonly message: string;
  readonly field?: string;
}> {
  readonly publicErrorCategory = "validation" as const;
}

export class Forbidden extends Data.TaggedError("Forbidden")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "forbidden" as const;
}

export class Unauthorized extends Data.TaggedError("Unauthorized") {
  readonly publicErrorCategory = "unauthorized" as const;
}

export const PublicErrorCategory = Schema.Literals([
  "validation",
  "unauthorized",
  "forbidden",
  "not-found",
  "conflict",
  "internal",
]);

export type PublicErrorCategory = typeof PublicErrorCategory.Type;

const encode = Schema.encodeSync(PublicError);
const decodePublicFailure = Schema.decodeUnknownOption(
  Schema.Struct({
    publicErrorCategory: PublicErrorCategory,
  }),
);
const decodeTaggedFailure = Schema.decodeUnknownOption(
  Schema.Struct({ _tag: Schema.String }),
);
const decodeValidationIssues = Schema.decodeUnknownOption(
  Schema.Array(Schema.Struct({ message: Schema.String })),
);

function isTanStackValidationError(message: string | undefined) {
  if (message === undefined) return false;
  try {
    const issues = decodeValidationIssues(JSON.parse(message));
    return Option.isSome(issues) && issues.value.length > 0;
  } catch {
    return false;
  }
}

function publicCategory(cause: unknown): PublicErrorCategory {
  const failure = decodePublicFailure(cause);
  if (Option.isSome(failure)) return failure.value.publicErrorCategory;
  const message = cause instanceof Error ? cause.message : undefined;
  return isTanStackValidationError(message) ? "validation" : "internal";
}

function isExpectedFailure(cause: unknown): boolean {
  if (
    Option.isSome(decodePublicFailure(cause)) ||
    Option.isSome(decodeTaggedFailure(cause))
  ) {
    return true;
  }
  return isTanStackValidationError(
    cause instanceof Error ? cause.message : undefined,
  );
}

type PublicErrorReport = (
  kind: "defect" | "interruption",
  cause: unknown,
) => void;

function defaultReport(kind: "defect" | "interruption", cause: unknown) {
  Effect.runFork(Effect.logError(`Public boundary ${kind}.`, cause));
}

function reportUnexpectedCause(
  cause: unknown,
  report: PublicErrorReport = defaultReport,
) {
  if (Cause.isCause(cause)) {
    let reported = false;
    if (Cause.hasDies(cause)) {
      report("defect", cause);
      reported = true;
    }
    if (Cause.hasInterrupts(cause)) {
      report("interruption", cause);
      reported = true;
    }
    if (reported) return;
  }
  if (!isExpectedFailure(cause)) report("defect", cause);
}

export function encodePublicError(cause: unknown): PublicErrorData {
  switch (publicCategory(cause)) {
    case "validation":
      return encode({
        _tag: "PublicError",
        code: "VALIDATION_FAILED",
        message: "The request is invalid.",
      });
    case "unauthorized":
      return encode({
        _tag: "PublicError",
        code: "UNAUTHORIZED",
        message: "Authentication is required.",
      });
    case "forbidden":
      return encode({
        _tag: "PublicError",
        code: "FORBIDDEN",
        message: "You do not have permission to perform this action.",
      });
    case "not-found":
      return encode({
        _tag: "PublicError",
        code: "NOT_FOUND",
        message: "The requested resource was not found.",
      });
    case "conflict":
      return encode({
        _tag: "PublicError",
        code: "CONFLICT",
        message: "The request conflicts with the current state.",
      });
    case "internal":
      return encode({
        _tag: "PublicError",
        code: "INTERNAL",
        message: "The request could not be completed.",
      });
  }
}

export function encodePublicBoundaryError(
  cause: unknown,
  init?: { readonly report?: PublicErrorReport },
): PublicErrorData {
  reportUnexpectedCause(cause, init?.report);
  return encodePublicError(cause);
}

export function statusForPublicError(error: PublicErrorData): number {
  switch (error.code) {
    case "VALIDATION_FAILED":
      return 422;
    case "UNAUTHORIZED":
      return 401;
    case "FORBIDDEN":
      return 403;
    case "NOT_FOUND":
      return 404;
    case "CONFLICT":
      return 409;
    case "INTERNAL":
      return 500;
  }
}

export function publicErrorResponse(
  cause: unknown,
  init?: {
    readonly headers?: HeadersInit;
    readonly status?: number;
    readonly report?: (
      kind: "defect" | "interruption",
      cause: unknown,
    ) => void;
  },
) {
  const error = encodePublicBoundaryError(cause, { report: init?.report });
  return Response.json(error, {
    status: init?.status ?? statusForPublicError(error),
    headers: init?.headers,
  });
}
