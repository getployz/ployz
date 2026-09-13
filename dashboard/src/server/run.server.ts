import "@tanstack/react-start/server-only";
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
  if (Schema.isSchemaError(cause)) return true;
  const evidence = parseErrorEvidence(
    cause instanceof Error ? cause : asRecord(cause),
  );
  if (evidence.retriable === false) return true;
  const failure = decodePublicFailure(cause);
  return (
    Option.isSome(failure) &&
    NON_RETRIABLE_PUBLIC_ERROR_CATEGORIES.has(failure.value.publicErrorCategory)
  );
}

export function makeEffectRunner<R, ER>(runtime: Runtime<R, ER>) {
  return async <A, E>(
    program: Effect.Effect<A, E, R>,
    options?: Effect.RunOptions,
  ): Promise<A> => {
    const exit = await runtime.runPromiseExit(program, options);
    if (Exit.isSuccess(exit)) return exit.value;

    if (!Cause.hasDies(exit.cause) && !Cause.hasInterrupts(exit.cause)) {
      const failure = Cause.findErrorOption(exit.cause);
      if (Option.isSome(failure)) throw failure.value;
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
      if (cause instanceof NonRetriableError) throw cause;
      if (isNonRetriableInngestCause(cause)) {
        throw new NonRetriableError(
          cause instanceof Error ? cause.message : "Effect activity failed.",
          { cause: cause instanceof Error ? cause : undefined },
        );
      }
      if (cause instanceof Error) throw cause;
      throw new Error("Effect activity failed.", { cause });
    }
  };
}

export const runInngestEffect: <A, E extends Error>(
  program: Effect.Effect<A, E, AppServices>,
) => Promise<A> = makeInngestEffectRunner(runAppEffect);
