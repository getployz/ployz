import "@tanstack/react-start/server-only";
import * as OtelTracer from "@effect/opentelemetry/OtelTracer";
import { context as otelContext, trace as otelTrace } from "@opentelemetry/api";
import { Cause, Effect, Exit, Option, Schema } from "effect";
import { NonRetriableError } from "inngest";
import type { ManagedRuntime } from "effect";
import { parseErrorEvidence } from "#/lib/error-evidence";
import { asRecord } from "#/lib/json";
import { PublicErrorCategory } from "#/server/public-error";
import { AppRuntime, type AppServices } from "#/server/runtime.server";

type Runtime<R, ER> = Pick<ManagedRuntime.ManagedRuntime<R, ER>, "runPromiseExit">;

const NON_RETRIABLE_PUBLIC_ERROR_CATEGORIES = new Set([
  "conflict",
  "validation",
  "not-found",
]);

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
  return (
    Option.isSome(decoded) &&
    NON_RETRIABLE_PUBLIC_ERROR_CATEGORIES.has(decoded.value.publicErrorCategory)
  );
}

function describeCause(cause: Cause.Cause<unknown>) {
  const squashed = Cause.squash(cause);
  return squashed instanceof Error && squashed.message.length > 0
    ? squashed.message
    : Cause.pretty(cause);
}

/**
 * Runs an Effect on a managed runtime and rethrows its failure for
 * Promise-based callers.
 *
 * A typed failure is thrown as-is, even when the cause also carries a defect
 * or interruption (for example a finalizer that died while unwinding): the
 * typed failure is the primary signal and keeps public-error categories and
 * Inngest retry classification intact. The shadowed defect is logged so it is
 * not lost. A cause with no typed failure is thrown as the raw `Cause` so
 * boundaries can report defects and interruptions distinctly.
 */
export function makeEffectRunner<R, ER>(runtime: Runtime<R, ER>) {
  return async <A, E>(
    program: Effect.Effect<A, E, R>,
    options?: Effect.RunOptions,
  ): Promise<A> => {
    // Fibers run on Effect's scheduler, outside the request's async context,
    // so the OpenTelemetry parent (the HTTP server span) is captured here.
    const parent = otelTrace.getSpanContext(otelContext.active());
    const exit = await runtime.runPromiseExit(
      parent === undefined ? program : OtelTracer.withSpanContext(program, parent),
      options,
    );
    if (Exit.isSuccess(exit)) return exit.value;

    const failure = Cause.findErrorOption(exit.cause);
    if (Option.isSome(failure)) {
      if (Cause.hasDies(exit.cause)) {
        Effect.runFork(
          Effect.logError("Defect shadowed by a typed failure.", exit.cause),
        );
      }
      throw failure.value;
    }
    throw exit.cause;
  };
}

export const runAppEffect = makeEffectRunner(AppRuntime);

type EffectRunner<R> = <A, E>(
  program: Effect.Effect<A, E, R>,
  options?: Effect.RunOptions,
) => Promise<A>;

export function makeInngestEffectRunner<R>(runEffect: EffectRunner<R>) {
  return async <A, E extends Error>(program: Effect.Effect<A, E, R>) => {
    try {
      return await runEffect(program);
    } catch (cause) {
      const failure = Cause.isCause(cause) ? Cause.squash(cause) : cause;
      if (failure instanceof NonRetriableError) throw failure;
      const message = Cause.isCause(cause)
        ? describeCause(cause)
        : cause instanceof Error
          ? cause.message
          : "Effect activity failed.";
      if (isNonRetriableInngestCause(cause)) {
        throw new NonRetriableError(message, { cause });
      }
      if (cause instanceof Error) throw cause;
      throw new Error(message, { cause });
    }
  };
}

export const runInngestEffect: <A, E extends Error>(
  program: Effect.Effect<A, E, AppServices>,
) => Promise<A> = makeInngestEffectRunner(runAppEffect);
