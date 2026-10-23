import { Cause, Context, Effect, Exit, Fiber, Layer, Logger, References, Tracer } from "effect";
import { holdsQuery, loggableCause, mayCarryQuery, safeSummary } from "#/server/public-error";

/** How deep into a logged record or array [`redact`] reads before it redacts what is left. */
const FIELD_DEPTH = 3;

function isPlainRecord(cause: unknown): cause is object {
  if (cause === null || cause === undefined || Object(cause) !== cause) return false;
  if (Array.isArray(cause) || Cause.isCause(cause) || cause instanceof Error) return false;
  const prototype: unknown = Object.getPrototypeOf(cause);
  return prototype === Object.prototype || prototype === null;
}

/**
 * `value` as a log may hold it: whole unless a failed query may be somewhere in it. Then a plain record or array is
 * redacted field by field, a getter's field and anything unreadable as `[redacted]`, and an error by
 * [`safeSummary`]. A failed query's message holds its params.
 */
// A log message is whatever its caller passed, so what this hands back is too.
// oxlint-disable-next-line anti-slop/no-unknown-returns
function redact(cause: unknown, depth: number): unknown {
  try {
    if (!holdsQuery(cause)) return cause;
    if (Cause.isCause(cause)) return loggableCause(cause);
    if (mayCarryQuery(cause)) return safeSummary(cause);
    if (depth < FIELD_DEPTH && Array.isArray(cause)) return cause.map((item) => redact(item, depth + 1));
    if (depth < FIELD_DEPTH && isPlainRecord(cause)) return redactFields(cause, depth);
    // oxlint-disable-next-line anti-slop/no-known-value-widening
    return cause instanceof Error ? safeSummary(cause) : "[redacted]";
  } catch {
    // oxlint-disable-next-line anti-slop/no-known-value-widening
    return "[redacted]";
  }
}

/** Each own string-keyed field of `record` by [`redact`], without calling a getter. */
// A logged record is whatever its caller passed, so its fields are too.
// oxlint-disable-next-line anti-slop/no-object-parameters, anti-slop/no-unsafe-dictionary-type
function redactFields(record: object, depth: number): Record<string, unknown> {
  return Object.fromEntries(Object.keys(record).map((key) => {
    const field = Reflect.getOwnPropertyDescriptor(record, key);
    return [key, field?.get === undefined ? redact(field?.value, depth + 1) : "[redacted]"];
  }));
}

/** `cause`, or a defect naming its reasons when a failed query may be somewhere in it. */
function redactedCause(cause: Cause.Cause<unknown>): Cause.Cause<unknown> {
  try {
    const loggable = loggableCause(cause);
    return Cause.isCause(loggable) ? loggable : Cause.die(loggable);
  } catch {
    return Cause.die("[redacted]");
  }
}

/**
 * `fiber` as a logger reads it, with its log annotations redacted: the default loggers print them beside the message.
 * The fiber itself when none may hold a failed query.
 */
function redactedFiber(fiber: Fiber.Fiber<unknown, unknown>): Fiber.Fiber<unknown, unknown> {
  const annotations = fiber.getRef(References.CurrentLogAnnotations);
  if (!holdsQuery(annotations)) return fiber;
  const context = Context.add(fiber.context, References.CurrentLogAnnotations, redactFields(annotations, 0));
  return new Proxy(fiber, {
    get(target, key) {
      if (key === "getRef") return <A>(ref: Context.Reference<A>) => Context.get(context, ref);
      if (key === "context") return context;
      // A logger reads any of the fiber's members; each is the fiber's own, read with the fiber as `this`.
      // oxlint-disable-next-line anti-slop/no-reflect-get
      const value: unknown = Reflect.get(target, key, target);
      return value instanceof Function ? value.bind(target) : value;
    },
  });
}

/**
 * One logger that hands each of `loggers` the log with every failed query named by its kind, never its params.
 * The log is redacted once, however many loggers read it.
 */
export function redactingLogger(loggers: Iterable<Logger.Logger<unknown, unknown>>): Logger.Logger<unknown, void> {
  const targets = [...loggers];
  return Logger.make((options) => {
    const redacted = {
      ...options,
      message: redact(options.message, 0),
      cause: redactedCause(options.cause),
      fiber: redactedFiber(options.fiber),
    };
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
