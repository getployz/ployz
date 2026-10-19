import { Schema } from "effect";
import { PublicError } from "#/lib/public-error";

const isPublicError = Schema.is(PublicError);

/** A request that never got an answer (offline, Cloud restarting): the browser's words ("Failed to fetch") mean nothing here. */
const UNREACHABLE = "Couldn't reach Ployz Cloud. Check your connection and try again.";

/** A server function rejects with an encoded PublicError, a plain object, not an Error. */
export function toErrorMessage<T>(error: T, fallback: string): string {
  if (error instanceof TypeError) return UNREACHABLE;
  if (error instanceof Error) return error.message;
  return isPublicError(error) ? error.message : fallback;
}

export function isNotFound<T>(error: T) {
  return isPublicError(error) && error.code === "NOT_FOUND";
}

/** The session ended: the way on is signing in again. */
export function isUnauthorized<T>(error: T) {
  return isPublicError(error) && error.code === "UNAUTHORIZED";
}

export function describeFailureCause(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}
