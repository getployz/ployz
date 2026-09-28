import { Schema } from "effect";
import { PublicError } from "#/lib/public-error";

const isPublicError = Schema.is(PublicError);

/** A server function rejects with an encoded PublicError, a plain object, not an Error. */
export function toErrorMessage<T>(error: T, fallback: string): string {
  if (error instanceof Error) return error.message;
  return isPublicError(error) ? error.message : fallback;
}

export function describeFailureCause(cause: unknown): string {
  return cause instanceof Error ? cause.message : String(cause);
}
