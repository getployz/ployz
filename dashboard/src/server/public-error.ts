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

export const PublicErrorCategory = Schema.Literals([
  "validation",
  "unauthorized",
  "forbidden",
  "not-found",
  "conflict",
  "rate-limited",
  "build-grant-unavailable",
  "unavailable",
  "server-lacks",
  "internal",
]);

export type PublicErrorCategory = typeof PublicErrorCategory.Type;

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
const decodeTaggedFailure = Schema.decodeUnknownOption(
  Schema.Struct({ _tag: Schema.String }),
);
const decodeErrorCode = Schema.decodeUnknownOption(
  Schema.Struct({ code: Schema.String }),
);
const decodeNestedCause = Schema.decodeUnknownOption(
  Schema.Struct({ cause: Schema.Unknown }),
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
  return publicCategory(cause) !== "internal";
}

const IDENTIFIER = /^[A-Za-z][A-Za-z0-9_]{0,63}$/u;
const SQLSTATE = /^[0-9A-Z]{5}$/u;

/**
 * A typed failure that answers 500, named by its tags and SQLSTATE alone: its fields, such as a failed query's params,
 * can hold secrets, and so can a tag or code that isn't shaped like one.
 */
function typedFailureSummary(cause: unknown): string {
  const tags: string[] = [];
  let code: string | undefined;
  for (const at of causeChain(cause)) {
    const tagged = decodeTaggedFailure(at);
    if (Option.isSome(tagged)) tags.push(IDENTIFIER.test(tagged.value._tag) ? tagged.value._tag : "?");
    const found = Option.getOrUndefined(decodeErrorCode(at))?.code;
    if (found !== undefined && SQLSTATE.test(found)) code ??= found;
  }
  return code === undefined ? tags.join(" > ") : `${tags.join(" > ")} (${code})`;
}

/** `cause`, then each `cause` it wraps, a few deep. */
function* causeChain(cause: unknown) {
  for (let at: unknown = cause, depth = 0; at !== undefined && depth < 8; depth += 1) {
    yield at;
    const next = Option.getOrUndefined(decodeNestedCause(at))?.cause;
    at = Cause.isCause(next) ? Cause.squash(next) : next;
  }
}

const QUERY_FAILURES = new Set(["EffectDrizzleQueryError", "SqlError"]);
const decodeFailedQuery = Schema.decodeUnknownOption(
  Schema.Struct({ _tag: Schema.Literal("EffectDrizzleQueryError"), query: Schema.String }),
);
const STATEMENT = /\b(insert\s+into|delete\s+from|update|from)\s+((?:"[A-Za-z_][A-Za-z0-9_]*"|[A-Za-z_][A-Za-z0-9_]*)(?:\.(?:"[A-Za-z_][A-Za-z0-9_]*"|[A-Za-z_][A-Za-z0-9_]*))?)/iu;
const VERB = /^\s*(?:with\b[\s\S]*?\)\s*)?(select|insert|update|delete)\b/iu;

/** A failed query's statement as its verb and first table, such as `insert machine_enrollment_token`. */
function statementName(query: string): string | undefined {
  const verb = VERB.exec(query)?.[1] ?? STATEMENT.exec(query)?.[1]?.split(/\s/u)[0];
  const table = STATEMENT.exec(query)?.[2]?.replaceAll('"', "");
  if (verb === undefined || table === undefined) return undefined;
  return `${verb.toLowerCase()} ${table}`;
}

/**
 * A value that carries a failed query, named by its tags, SQLSTATE and statement: the query's message holds its
 * params, and the database's message can quote the values it refused.
 */
function failedQuerySummary(cause: unknown): string | undefined {
  const chain = [...causeChain(cause)];
  const carriesQuery = chain.some((at) => {
    const tagged = decodeTaggedFailure(at);
    return Option.isSome(tagged) && QUERY_FAILURES.has(tagged.value._tag);
  });
  if (!carriesQuery) return undefined;
  const query = chain.map((at) => Option.getOrUndefined(decodeFailedQuery(at))?.query).find((at) => at !== undefined);
  const statement = query === undefined ? undefined : statementName(query);
  const summary = typedFailureSummary(cause);
  return statement === undefined ? summary : `${summary} in ${statement}`;
}

/**
 * What a log may hold of `cause`: all of it, unless a failure or defect in it carries a failed query. Then each
 * reason is named by [`failedQuerySummary`] or, when it carries none, by its pretty form.
 */
export function loggableCause(cause: Cause.Cause<unknown>): Cause.Cause<unknown> | string {
  const reasons = cause.reasons.map((reason) => {
    const value = Cause.isDieReason(reason) ? reason.defect : Cause.isFailReason(reason) ? reason.error : undefined;
    return { reason, summary: value === undefined ? undefined : failedQuerySummary(value) };
  });
  if (reasons.every(({ summary }) => summary === undefined)) return cause;
  return reasons
    .map(({ reason, summary }) => summary ?? Cause.pretty(Cause.fromReasons([reason])))
    .join("\n");
}

type PublicErrorReport = (
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
  Effect.runFork(log(`Public boundary ${kind}.`, cause));
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
      report("interruption", cause);
      reported = true;
    }
    if (reported) return;
  }
  if (isExpectedFailure(cause)) return;
  report("defect", Option.isSome(decodeTaggedFailure(cause)) ? typedFailureSummary(cause) : cause);
}

function userFacingMessage(cause: unknown, fallback: string): string {
  const failure = decodeUserFacingMessage(cause);
  return Option.isSome(failure) ? failure.value.message : fallback;
}

const internal = () => encode({
  _tag: "PublicError",
  code: "INTERNAL",
  message: "The request could not be completed.",
});

export function encodePublicError(cause: unknown): PublicErrorData {
  switch (publicCategory(cause)) {
    case "validation":
      return encode({
        _tag: "PublicError",
        code: "VALIDATION_FAILED",
        message: userFacingMessage(cause, "The request is invalid."),
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
        message: userFacingMessage(cause, "You do not have permission to perform this action."),
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
        message: userFacingMessage(cause, "The request conflicts with the current state."),
      });
    case "rate-limited":
      return encode({
        _tag: "PublicError",
        code: "RATE_LIMITED",
        message: userFacingMessage(cause, "Too many requests. Try again later."),
      });
    case "build-grant-unavailable":
      return encode({
        _tag: "PublicError",
        code: "BUILD_GRANT_UNAVAILABLE",
        message: "Your Cluster could not mint a Build Grant.",
      });
    case "unavailable":
      return encode({
        _tag: "PublicError",
        code: "UNAVAILABLE",
        message: userFacingMessage(cause, "A required service is unavailable. Try again shortly."),
      });
    case "server-lacks": {
      const lack = decodeServerLacks(cause);
      if (Option.isNone(lack)) return internal();
      return encode({
        _tag: "PublicError",
        code: "SERVER_LACKS",
        message: "A server needs a newer Ployz.",
        ...lack.value,
      });
    }
    case "internal":
      return internal();
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
    case "SERVER_LACKS":
      return 409;
    case "RATE_LIMITED":
      return 429;
    case "BUILD_GRANT_UNAVAILABLE":
    case "UNAVAILABLE":
      return 503;
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
  const headers = new Headers(init?.headers);
  if (cause instanceof RateLimited && cause.retryAfterSeconds !== undefined && !headers.has("retry-after")) {
    headers.set("retry-after", String(cause.retryAfterSeconds));
  }
  return Response.json(error, {
    status: init?.status ?? statusForPublicError(error),
    headers,
  });
}

/**
 * The `ANY` answer of a route, for a method none of its handlers takes. Without it, that method falls through to the
 * page and answers an API client with the app's HTML shell. `Allow` names the route's own handlers.
 */
export function methodNotAllowed(route: { options: { server?: { handlers?: object } } }): Response {
  const methods = Object.keys(route.options.server?.handlers ?? {}).filter((method) => method !== "ANY");
  const allow = methods.includes("GET") && !methods.includes("HEAD") ? [...methods, "HEAD"] : methods;
  return publicErrorResponse(new NotFound({ message: `This path takes ${allow.join(", ")}.` }), {
    status: 405,
    headers: { Allow: allow.join(", ") },
  });
}
