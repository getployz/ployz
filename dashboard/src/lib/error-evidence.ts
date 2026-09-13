import type { JsonObject } from "#/db/tables";
import { asBoolean, asFiniteNumber, asRecord, asString } from "#/lib/json";
import { Cause, Option } from "effect";

export interface ErrorEvidence {
  code?: string;
  constraint?: string;
  failureCode?: string;
  retriable?: boolean;
  retryAfterSeconds?: number;
  message?: string;
}

function evidenceField(value: Error | JsonObject, key: keyof ErrorEvidence | "cause") {
  if (!(key in value)) return undefined;
  // SAFETY: Error subclasses keep constraint/code on the instance; JSON
  // projection via asRecord drops those class fields.
  return (value as JsonObject)[key];
}

export function parseErrorEvidence(
  cause: Error | JsonObject | null | undefined,
  maxDepth = 5,
): ErrorEvidence {
  const evidence: ErrorEvidence = {};
  let current: Error | JsonObject | null | undefined = cause;
  for (let depth = 0; depth < maxDepth && current != null; depth += 1) {
    if (current instanceof Error && evidence.message === undefined) {
      evidence.message = current.message;
    }
    const record = {
      ...(asRecord(current) ?? {}),
      code: evidenceField(current, "code"),
      constraint: evidenceField(current, "constraint"),
      failureCode: evidenceField(current, "failureCode"),
      retriable: evidenceField(current, "retriable"),
      retryAfterSeconds: evidenceField(current, "retryAfterSeconds"),
      message: evidenceField(current, "message"),
      cause: evidenceField(current, "cause") ??
        (current instanceof Error ? current.cause : undefined),
    };
    evidence.code ??= asString(record.code) ?? undefined;
    evidence.constraint ??= asString(record.constraint) ?? undefined;
    evidence.failureCode ??= asString(record.failureCode) ?? undefined;
    evidence.retriable ??= asBoolean(record.retriable) ?? undefined;
    evidence.retryAfterSeconds ??=
      asFiniteNumber(record.retryAfterSeconds) ?? undefined;
    evidence.message ??= asString(record.message) ?? undefined;
    const nestedCause = record.cause;
    const failure = Cause.isCause(nestedCause)
      ? Cause.findErrorOption(nestedCause)
      : Option.none();
    current = Option.isSome(failure)
      ? failure.value instanceof Error
        ? failure.value
        : asRecord(failure.value)
      : nestedCause instanceof Error
        ? nestedCause
        : asRecord(nestedCause);
  }
  return evidence;
}

export function isUniqueViolation(
  cause: Error | JsonObject | null | undefined,
) {
  return parseErrorEvidence(cause).code === "23505";
}

export function findErrorEvidence(
  cause: Error | JsonObject | null | undefined,
  match: (evidence: ErrorEvidence) => boolean,
  maxDepth = 5,
): ErrorEvidence | null {
  let current: Error | JsonObject | null | undefined = cause;
  for (let depth = 0; depth < maxDepth && current != null; depth += 1) {
    const evidence = parseErrorEvidence(current, 1);
    if (match(evidence)) return evidence;
    const nestedCause =
      current instanceof Error ? current.cause : asRecord(current)?.["cause"];
    current =
      nestedCause instanceof Error ? nestedCause : asRecord(nestedCause);
  }
  return null;
}

export function errorEvidenceFrom<T>(error: T) {
  return parseErrorEvidence(error instanceof Error ? error : asRecord(error));
}
