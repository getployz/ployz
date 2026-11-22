import { Cause, Data, Effect, Option, Schema } from "effect";
import {
  PUBLIC_ERROR_CODES,
  PublicError,
  PublicErrorCategory,
  type PublicError as PublicErrorData,
} from "#/lib/public-error";
import { holdsQuery, isTaggedFailure, loggableCause, runRedactedLog, safeSummary, typedFailureSummary } from "#/server/log-redaction";
import type { CommandRefusal } from "#/modules/config-store/store.contract";
import type { StoreRefusal } from "#/lib/refusal";

export class NotFound extends Data.TaggedError("NotFound")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "not-found" as const;
}

/**
 * `userFacing: true` sends `message` to the client verbatim instead of the generic copy. Only set it on a fixed sentence
 * written for users (user-chosen names are fine): never ids, provider output, or causes.
 */
type UserFacing = { readonly userFacing?: true };

export class Conflict extends Data.TaggedError("Conflict")<{
  readonly message: string;
} & UserFacing> {
  readonly publicErrorCategory = "conflict" as const;
}

export class Validation extends Data.TaggedError("Validation")<{
  readonly message: string;
  readonly field?: string;
} & UserFacing> {
  readonly publicErrorCategory = "validation" as const;
}

/** `retryAfterSeconds`, when set, goes out as the reply's `Retry-After`. */
export class RateLimited extends Data.TaggedError("RateLimited")<{
  readonly message: string;
  readonly retryAfterSeconds?: number;
} & UserFacing> {
  readonly publicErrorCategory = "rate-limited" as const;
}

export class Forbidden extends Data.TaggedError("Forbidden")<{
  readonly message: string;
} & UserFacing> {
  readonly publicErrorCategory = "forbidden" as const;
}

export class Unauthorized extends Data.TaggedError("Unauthorized") {
  readonly publicErrorCategory = "unauthorized" as const;
}

/** The Cluster could not mint a GitHub run's Build Grant. The cause stays in the server log. */
export class BuildGrantUnavailable extends Data.TaggedError("BuildGrantUnavailable")<{
  readonly cause: unknown;
}> {
  readonly publicErrorCategory = "build-grant-unavailable" as const;
}

/** A dependency Cloud needs, such as a Machine it must reach, is not answering yet; the caller retries. */
export class Unavailable extends Data.TaggedError("Unavailable")<{
  readonly message: string;
  readonly cause?: unknown;
} & UserFacing> {
  readonly publicErrorCategory = "unavailable" as const;
}

interface PublicErrorRow {
  readonly status: number;
  /** Whether a background job that fails this way is worth another attempt. */
  readonly retriable: boolean;
  /** What a client reads, unless an error marked `userFacing` carries its own sentence. */
  readonly message: string;
  readonly userFacing: boolean;
}

/** What each category answers and how a retry treats it. Its wire code is in `PUBLIC_ERROR_CODES`. */
const PUBLIC_ERRORS = {
  validation: { status: 422, retriable: false, message: "The request is invalid.", userFacing: true },
  unauthorized: { status: 401, retriable: true, message: "Authentication is required.", userFacing: false },
  forbidden: { status: 403, retriable: true, message: "You do not have permission to perform this action.", userFacing: true },
  "not-found": { status: 404, retriable: false, message: "The requested resource was not found.", userFacing: false },
  conflict: { status: 409, retriable: false, message: "The request conflicts with the current state.", userFacing: true },
  "rate-limited": { status: 429, retriable: true, message: "Too many requests. Try again later.", userFacing: true },
  "build-grant-unavailable": { status: 503, retriable: true, message: "Your Cluster could not mint a Build Grant.", userFacing: false },
  unavailable: { status: 503, retriable: true, message: "A required service is unavailable. Try again shortly.", userFacing: true },
  "server-lacks": { status: 409, retriable: true, message: "A server needs a newer Ployz.", userFacing: false },
  internal: { status: 500, retriable: true, message: "The request could not be completed.", userFacing: false },
} as const satisfies Record<PublicErrorCategory, PublicErrorRow>;

/** Whether an Inngest function that fails with an error of `category` is worth retrying. */
export const isRetriableCategory = (category: PublicErrorCategory) => PUBLIC_ERRORS[category].retriable;

const encode = Schema.encodeSync(PublicError);
const decodePublicFailure = Schema.decodeUnknownOption(
  Schema.Struct({
    publicErrorCategory: PublicErrorCategory,
  }),
);
const decodeUserFacingMessage = Schema.decodeUnknownOption(
  Schema.Struct({ userFacing: Schema.Literal(true), message: Schema.String }),
);
const decodeServerLacks = Schema.decodeUnknownOption(
  Schema.Struct({ machineId: Schema.String, capability: Schema.String }),
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
    // A message that isn't JSON isn't TanStack's list of validation issues.
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
  return publicCategory(cause) !== "internal";
}

export type PublicErrorReport = (
  kind: "defect" | "interruption",
  cause: unknown,
) => void;

/** The client went away: an interrupted request, or an SDK or fetch call aborted with it. */
export function isClientDisconnect(cause: unknown): boolean {
  const failure = Cause.isCause(cause) ? Cause.squash(cause) : cause;
  return Cause.isCause(cause) && Cause.hasInterruptsOnly(cause)
    || failure instanceof Error && (failure.name === "AbortError" || failure.message === "aborted");
}

function defaultReport(kind: "defect" | "interruption", cause: unknown) {
  const log = kind === "interruption" || isClientDisconnect(cause) ? Effect.logDebug : Effect.logError;
  runRedactedLog(log(`Public boundary ${kind}.`, cause));
}

function reportUnexpectedCause(
  cause: unknown,
  report: PublicErrorReport = defaultReport,
) {
  if (Cause.isCause(cause)) {
    let reported = false;
    if (Cause.hasDies(cause)) {
      report("defect", loggableCause(cause));
      reported = true;
    }
    if (Cause.hasInterrupts(cause)) {
      report("interruption", loggableCause(cause));
      reported = true;
    }
    if (reported) return;
  }
  if (isExpectedFailure(cause)) return;
  report(
    "defect",
    isTaggedFailure(cause) ? typedFailureSummary(cause) : holdsQuery(cause) ? safeSummary(cause) : cause,
  );
}

function userFacingMessage(cause: unknown, fallback: string): string {
  const failure = decodeUserFacingMessage(cause);
  return Option.isSome(failure) ? failure.value.message : fallback;
}

function internal() {
  return encode({ _tag: "PublicError", code: "INTERNAL", message: PUBLIC_ERRORS.internal.message });
}

export function encodePublicError(cause: unknown): PublicErrorData {
  const category = publicCategory(cause);
  const row = PUBLIC_ERRORS[category];
  const message = row.userFacing ? userFacingMessage(cause, row.message) : row.message;
  if (category === "server-lacks") {
    const lack = decodeServerLacks(cause);
    if (Option.isNone(lack)) return internal();
    return encode({ _tag: "PublicError", code: PUBLIC_ERROR_CODES[category], message, ...lack.value });
  }
  return encode({ _tag: "PublicError", code: PUBLIC_ERROR_CODES[category], message });
}

export function encodePublicBoundaryError(
  cause: unknown,
  init?: { readonly report?: PublicErrorReport },
): PublicErrorData {
  reportUnexpectedCause(cause, init?.report);
  return encodePublicError(cause);
}

export function statusForPublicError(error: PublicErrorData): number {
  const category = PublicErrorCategory.literals.find((at) => PUBLIC_ERROR_CODES[at] === error.code) ?? "internal";
  return PUBLIC_ERRORS[category].status;
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
  const headers = new Headers(init?.headers);
  if (cause instanceof RateLimited && cause.retryAfterSeconds !== undefined && !headers.has("retry-after")) {
    headers.set("retry-after", String(cause.retryAfterSeconds));
  }
  return Response.json(error, {
    status: init?.status ?? statusForPublicError(error),
    headers,
  });
}

/** The 405 of a route whose handlers take `methods`, for a method none of them takes. */
export function methodNotAllowedFor(methods: ReadonlyArray<string>): Response {
  const allow = methods.includes("GET") && !methods.includes("HEAD") ? [...methods, "HEAD"] : methods;
  return publicErrorResponse(new NotFound({ message: `This path takes ${allow.join(", ")}.` }), {
    status: 405,
    headers: { Allow: allow.join(", ") },
  });
}

const noStore = { "cache-control": "no-store" };

/** A machine-facing refusal: `{ error }`, never cached, with whatever headers the route adds. */
export function machineRefusal(status: number, error: string, headers: Record<string, string> = {}) {
  return Response.json({ error }, { status, headers: { ...noStore, ...headers } });
}

const isUserFacing = Schema.is(Schema.Struct({ userFacing: Schema.Literal(true), message: Schema.String }));

/**
 * `cause` as a Machine's own API answers it: `{ error }` in the public error's status. `report` makes it a public
 * boundary that logs defects through it. `codeHeader` also names the code in `Ployz-Error-Code` for an error written
 * for users, and `details` goes in the body beside `error`.
 */
export function machineErrorResponse(
  cause: unknown,
  init: {
    readonly report?: PublicErrorReport;
    readonly codeHeader?: true;
    readonly details?: unknown;
  } = {},
) {
  const error = init.report === undefined ? encodePublicError(cause) : encodePublicBoundaryError(cause, { report: init.report });
  const headers = new Headers(noStore);
  if (init.codeHeader === true && isUserFacing(cause) && error.code !== "INTERNAL") headers.set("Ployz-Error-Code", error.code);
  return Response.json(
    init.details === undefined ? { error: error.message } : { error: error.message, details: init.details },
    { status: statusForPublicError(error), headers },
  );
}

const REFUSAL_STATUS: ReadonlyMap<string, number> = new Map([
  ["invalid_argument", 422],
  ["not_found", 404],
  ["unknown_command", 404],
  ["conflict", 409],
  ["ambiguous", 409],
  ["confirmation_required", 409],
  ["approval_required", 409],
  ["channel_mismatch", 409],
  ["device_request_conflict", 409],
  ["unauthenticated", 401],
  ["forbidden", 403],
  ["approval_denied", 403],
  ["claim_required", 403],
  ["unsupported", 501],
  ["unavailable", 503],
]);

/** A Store refusal travels to the CLI verbatim: the RPC error vocabulary, never the rejected value. */
export function statusForRefusal(code: string) {
  return REFUSAL_STATUS.get(code) ?? 500;
}

/** A Cloud command's refusal also names its `field`, what fits there and whether it wrote, as Store tools refuse. */
export function cliRefusalResponse(error: StoreRefusal | CommandRefusal) {
  const { code, message, details } = error;
  const told = "writes" in error ? { field: error.field, expected: error.expected, writes: error.writes } : {};
  return Response.json({ error: { code, message, details, ...told } }, {
    status: statusForRefusal(code),
    headers: noStore,
  });
}
