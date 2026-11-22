import "@tanstack/react-start/server-only";
import { Cause, Effect, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import { parseErrorEvidence } from "#/modules/inngest/error-evidence";
import { asRecord } from "#/lib/json";
import { runAppEffect } from "#/server/run.server";
import { PublicErrorCategory } from "#/lib/public-error";
import { loggableCause } from "#/server/log-redaction";
import { isRetriableCategory } from "#/server/public-error";
import type { AppServices } from "#/server/runtime.server";

const decodePublicFailure = Schema.decodeUnknownOption(
  Schema.Struct({
    publicErrorCategory: PublicErrorCategory,
  }),
);

export function isNonRetriableInngestCause(cause: unknown): boolean {
  // A raw `Cause` (thrown for defects and interruptions) is classified by its
  // squashed failure or defect value, matching what `Effect.runPromise` throws.
  const failure = Cause.isCause(cause) ? Cause.squash(cause) : cause;
  if (failure instanceof NonRetriableError) return true;
  if (Schema.isSchemaError(failure)) return true;
  const evidence = parseErrorEvidence(
    failure instanceof Error ? failure : asRecord(failure),
  );
  if (evidence.retriable === false) return true;
  const decoded = decodePublicFailure(failure);
  return Option.isSome(decoded) && !isRetriableCategory(decoded.value.publicErrorCategory);
}

function describeCause(cause: Cause.Cause<unknown>) {
  const loggable = loggableCause(cause);
  if (!Cause.isCause(loggable)) return loggable;
  const squashed = Cause.squash(cause);
  return squashed instanceof Error && squashed.message.length > 0
    ? squashed.message
    : Cause.pretty(cause);
}

/** The runner every Inngest function takes, so a test can swap in one with fewer services. */
export type EffectRunner<R = AppServices> = <A, E extends Error>(program: Effect.Effect<A, E, R>) => Promise<A>;

type AppEffectRunner<R> = <A, E>(
  program: Effect.Effect<A, E, R>,
  options?: Effect.RunOptions,
) => Promise<A>;

export function makeInngestEffectRunner<R>(runEffect: AppEffectRunner<R>) {
  return async <A, E extends Error>(program: Effect.Effect<A, E, R>) => {
    try {
      return await runEffect(program);
    } catch (cause) {
      const failure = Cause.isCause(cause) ? Cause.squash(cause) : cause;
      // Inngest keeps a thrown error's cause in its run history, so a redacted one goes without it.
      const redacted = Cause.isCause(cause) && !Cause.isCause(loggableCause(cause));
      if (failure instanceof NonRetriableError) {
        throw redacted ? new NonRetriableError(describeCause(cause)) : failure;
      }
      const message = Cause.isCause(cause)
        ? describeCause(cause)
        : cause instanceof Error
          ? cause.message
          : "Effect activity failed.";
      const attached = redacted ? {} : { cause };
      if (isNonRetriableInngestCause(cause)) {
        throw new NonRetriableError(message, attached);
      }
      if (cause instanceof Error) throw cause;
      throw new Error(message, attached);
    }
  };
}

export const runInngestEffect: EffectRunner = makeInngestEffectRunner(runAppEffect);
