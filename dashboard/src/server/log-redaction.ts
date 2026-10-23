import { Cause, Effect, Exit, Layer, Logger, Tracer } from "effect";
import { mayCarryQuery, safeSummary } from "#/server/public-error";

/** How deep into a logged record or array [`redact`] reads before it redacts what is left. */
const FIELD_DEPTH = 3;
/** How many fields [`holdsQuery`] reads before it treats a value as holding a query. */
const READABLE_FIELDS = 1024;

function isObject(cause: unknown): cause is object {
  return cause !== null && cause !== undefined && Object(cause) === cause;
}

function isPlainRecord(cause: unknown): cause is object {
  if (!isObject(cause) || Array.isArray(cause) || Cause.isCause(cause) || cause instanceof Error) return false;
  const prototype: unknown = Object.getPrototypeOf(cause);
  return prototype === Object.prototype || prototype === null;
}

/**
 * Whether a failed query is anywhere in `cause`: in a Cause, a `cause` chain, any own field, or a Map or Set entry.
 * A value too large or deep to read in [`READABLE_FIELDS`] fields counts as holding one. Getters are never called.
 */
function holdsQuery(cause: unknown): boolean {
  const seen = new Set<object>();
  const pending: unknown[] = [cause];
  for (let at = 0; at < pending.length; at += 1) {
    if (pending.length > READABLE_FIELDS) return true;
    const value = pending[at];
    if (!isObject(value) || seen.has(value)) continue;
    seen.add(value);
    if (mayCarryQuery(value)) return true;
    if (value instanceof Map) {
      for (const [key, entry] of value) pending.push(key, entry);
    } else if (value instanceof Set) {
      for (const entry of value) pending.push(entry);
    } else {
      for (const key of Object.getOwnPropertyNames(value)) pending.push(Object.getOwnPropertyDescriptor(value, key)?.value);
    }
  }
  return false;
}

/** `cause` with each reason named by [`safeSummary`], and an interruption as such. */
function summarizedCause(cause: Cause.Cause<unknown>): string {
  return cause.reasons
    .map((reason) =>
      Cause.isDieReason(reason)
        ? safeSummary(reason.defect)
        : Cause.isFailReason(reason)
          ? safeSummary(reason.error)
          : "interrupted"
    )
    .join("\n");
}

/**
 * `value` as a log may hold it: whole unless a failed query is somewhere in it. Then a plain record or array is
 * redacted field by field, and anything else is named by [`safeSummary`]. A failed query's message holds its params.
 */
// A log message is whatever its caller passed, so what this hands back is too.
// oxlint-disable-next-line anti-slop/no-unknown-returns
function redact(cause: unknown, depth: number): unknown {
  if (!holdsQuery(cause)) return cause;
  if (Cause.isCause(cause)) return summarizedCause(cause);
  if (mayCarryQuery(cause)) return safeSummary(cause);
  if (Array.isArray(cause)) return depth < FIELD_DEPTH ? cause.map((item) => redact(item, depth + 1)) : "[redacted]";
  if (isPlainRecord(cause)) {
    // oxlint-disable-next-line anti-slop/no-known-value-widening
    if (depth >= FIELD_DEPTH) return "[redacted]";
    return Object.fromEntries(Object.entries(cause).map(([key, field]) => [key, redact(field, depth + 1)]));
  }
  return safeSummary(cause);
}

/** `cause`, or a defect naming its reasons when a failed query is somewhere in it. */
function redactedCause(cause: Cause.Cause<unknown>): Cause.Cause<unknown> {
  return holdsQuery(cause) ? Cause.die(summarizedCause(cause)) : cause;
}

/**
 * One logger that hands each of `loggers` the log with every failed query named by its kind, never its params.
 * The log is redacted once, however many loggers read it.
 */
export function redactingLogger(loggers: Iterable<Logger.Logger<unknown, unknown>>): Logger.Logger<unknown, void> {
  const targets = [...loggers];
  return Logger.make((options) => {
    const redacted = { ...options, message: redact(options.message, 0), cause: redactedCause(options.cause) };
    for (const logger of targets) logger.log(redacted);
  });
}

/** Effect's default loggers, console and span, behind [`redactingLogger`]. */
export const redactedLoggers: ReadonlySet<Logger.Logger<unknown, unknown>> = new Set([
  redactingLogger([Logger.defaultLogger, Logger.tracerLogger]),
]);

export const RedactedLogsLive = Layer.succeed(Logger.CurrentLoggers, redactedLoggers);

/** Run a log outside the app runtime through [`redactedLoggers`]. */
export function runRedactedLog(log: Effect.Effect<void>) {
  return Effect.runFork(log.pipe(Effect.provideService(Logger.CurrentLoggers, redactedLoggers)));
}

/**
 * `tracer`, ending each span with its exit's failed queries named by their kind. A span that ends on a failure
 * records its errors and status message from that exit, and a failed query's message holds its params.
 */
export function redactingTracer(tracer: Tracer.Tracer): Tracer.Tracer {
  return Tracer.make({
    span(options) {
      const span = tracer.span(options);
      const end = span.end.bind(span);
      span.end = (endTime, exit) => end(endTime, Exit.isFailure(exit) ? Exit.failCause(redactedCause(exit.cause)) : exit);
      return span;
    },
    context: tracer.context,
  });
}

/** `tracing` with its tracer behind [`redactingTracer`]. */
export function redactSpanExits<Provided, E, R>(tracing: Layer.Layer<Provided, E, R>): Layer.Layer<Provided, E, R> {
  return Layer.effect(Tracer.Tracer, Effect.map(Effect.service(Tracer.Tracer), redactingTracer)).pipe(Layer.provideMerge(tracing));
}
