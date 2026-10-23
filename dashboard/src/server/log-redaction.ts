import { Cause, Effect, Layer, Logger } from "effect";
import { loggableCause, mayCarryQuery, safeSummary } from "#/server/public-error";

/** How deep into a logged record or array [`redact`] reads. */
const FIELD_DEPTH = 3;

function isPlainRecord(cause: unknown): cause is object {
  if (cause === null || Array.isArray(cause) || Cause.isCause(cause) || cause instanceof Error) return false;
  const prototype: unknown = Object.getPrototypeOf(cause);
  return prototype === Object.prototype || prototype === null;
}

/**
 * `value` as a log may hold it: a Cause by [`loggableCause`], anything else that may carry a failed query by its
 * summary, and a plain record or array field by field. A failed query's message holds its params.
 */
// A log message is whatever its caller passed, so what this hands back is too.
// oxlint-disable-next-line anti-slop/no-unknown-returns
function redact(cause: unknown, depth: number): unknown {
  if (Cause.isCause(cause)) return loggableCause(cause);
  if (Array.isArray(cause)) return depth < FIELD_DEPTH ? cause.map((item) => redact(item, depth + 1)) : cause;
  if (isPlainRecord(cause)) {
    if (depth >= FIELD_DEPTH) return mayCarryQuery(cause) ? "[redacted]" : cause;
    return Object.fromEntries(Object.entries(cause).map(([key, field]) => [key, redact(field, depth + 1)]));
  }
  return mayCarryQuery(cause) ? safeSummary(cause) : cause;
}

/** `logger`, given each log's message and cause with every failed query named by its kind, never its params. */
export function redactingLogger<Output>(logger: Logger.Logger<unknown, Output>): Logger.Logger<unknown, Output> {
  return Logger.make((options) => {
    const cause = loggableCause(options.cause);
    const message = redact(options.message, 0);
    return logger.log({
      ...options,
      message: Cause.isCause(cause) ? message : [...(Array.isArray(message) ? message : [message]), cause],
      cause: Cause.isCause(cause) ? cause : Cause.empty,
    });
  });
}

/** Effect's default loggers, console and span, each behind [`redactingLogger`]. */
export const redactedLoggers: ReadonlySet<Logger.Logger<unknown, unknown>> = new Set([
  redactingLogger(Logger.defaultLogger),
  redactingLogger(Logger.tracerLogger),
]);

export const RedactedLogsLive = Layer.succeed(Logger.CurrentLoggers, redactedLoggers);

/** Run a log outside the app runtime through [`redactedLoggers`]. */
export function runRedactedLog(log: Effect.Effect<void>) {
  return Effect.runFork(log.pipe(Effect.provideService(Logger.CurrentLoggers, redactedLoggers)));
}
