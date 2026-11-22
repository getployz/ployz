import "@tanstack/react-start/server-only";
import * as OtelTracer from "@effect/opentelemetry/OtelTracer";
import { context as otelContext, trace as otelTrace } from "@opentelemetry/api";
import { Cause, Effect, Exit, Option } from "effect";
import type { ManagedRuntime } from "effect";
import { runRedactedLog } from "#/server/log-redaction";
import { AppRuntime } from "#/server/runtime.server";

type Runtime<R, ER> = Pick<ManagedRuntime.ManagedRuntime<R, ER>, "runPromiseExit">;

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
        runRedactedLog(Effect.logError("Defect shadowed by a typed failure.", exit.cause));
      }
      throw failure.value;
    }
    throw exit.cause;
  };
}

export const runAppEffect = makeEffectRunner(AppRuntime);
