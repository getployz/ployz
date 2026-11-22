import { Cause, Context, Effect, Exit, Fiber, Layer, Logger, Option, References, Schema, Tracer } from "effect";

const decodeTaggedFailure = Schema.decodeUnknownOption(
  Schema.Struct({ _tag: Schema.String }),
);

/** Whether `cause` is a tagged failure, which a log names by its tag alone. */
export const isTaggedFailure = (cause: unknown) => Option.isSome(decodeTaggedFailure(cause));

const decodeErrorCode = Schema.decodeUnknownOption(
  Schema.Struct({ code: Schema.String }),
);
const decodeNestedCause = Schema.decodeUnknownOption(
  Schema.Struct({ cause: Schema.Unknown }),
);

const IDENTIFIER = /^[A-Za-z][A-Za-z0-9_]{0,63}$/u;
const SQLSTATE = /^[0-9A-Z]{5}$/u;

/**
 * A typed failure that answers 500, named by its tags and SQLSTATE alone: its fields, such as a failed query's params,
 * can hold secrets, and so can a tag or code that isn't shaped like one.
 */
export function typedFailureSummary(cause: unknown, untagged = ""): string {
  const tags: string[] = [];
  let code: string | undefined;
  for (const at of causeChain(cause)) {
    const tagged = decodeTaggedFailure(at);
    if (Option.isSome(tagged)) tags.push(IDENTIFIER.test(tagged.value._tag) ? tagged.value._tag : "?");
    const found = Option.getOrUndefined(decodeErrorCode(at))?.code;
    if (found !== undefined && SQLSTATE.test(found)) code ??= found;
  }
  const kind = tags.length > 0 ? tags.join(" > ") : untagged;
  return code === undefined ? kind : `${kind} (${code})`;
}

function* causeChain(cause: unknown) {
  for (let at: unknown = cause, depth = 0; at !== undefined && depth < 8; depth += 1) {
    yield at;
    const next = Option.getOrUndefined(decodeNestedCause(at))?.cause;
    at = Cause.isCause(next) ? Cause.squash(next) : next;
  }
}

const QUERY_FAILURES = new Set(["EffectDrizzleQueryError", "SqlError"]);
/** A failed query from Effect's drizzle, or from node-postgres drizzle, which Better Auth reads through. */
const decodeFailedQuery = Schema.decodeUnknownOption(Schema.Union([
  Schema.Struct({ _tag: Schema.Literal("EffectDrizzleQueryError"), query: Schema.String }),
  Schema.Struct({ name: Schema.Literal("DrizzleQueryError"), query: Schema.String }),
]));
const decodeErrorName = Schema.decodeUnknownOption(Schema.Struct({ name: Schema.String }));
/** A quote, dollar quote, escape or comment: text past one may be a value, so no statement is named. */
const UNREADABLE = /['\\`]|\$(?!\d)|\/\*|--|""/u;
const NAME = String.raw`(?:"[A-Za-z_][A-Za-z0-9_]*"|[A-Za-z_][A-Za-z0-9_]*)`;
const STATEMENT = new RegExp(String.raw`\b(?:insert\s+into|delete\s+from|update|from)\s+(${NAME}(?:\.${NAME})?)`, "iu");
const VERB = /^\s*(?:with\b[\s\S]*?\)\s*)?(select|insert|update|delete)\b/iu;
const READABLE_VALUES = 64;

/**
 * A failed query's statement as its verb and first table, such as `insert machine_enrollment_token`. None when the
 * query holds a literal, comment or anything but `$n` params and plain identifiers, since a value could pass for a table.
 */
function statementName(query: string): string | undefined {
  if (UNREADABLE.test(query)) return undefined;
  const verb = VERB.exec(query)?.[1];
  const table = STATEMENT.exec(query)?.[1]?.replaceAll('"', "");
  if (verb === undefined || table === undefined) return undefined;
  return `${verb.toLowerCase()} ${table}`;
}

/**
 * Every value in `cause`: each reason of a Cause, and each `cause` a value wraps, all branches. None when it holds
 * more than [`READABLE_VALUES`] values and Causes, so a cause too deep or cyclic to read is never taken as safe.
 */
function causeValues(cause: unknown): unknown[] | undefined {
  const values: unknown[] = [];
  const pending: unknown[] = [cause];
  let read = 0;
  for (let at = pending.shift(); at !== undefined || pending.length > 0; at = pending.shift()) {
    if (at === undefined) continue;
    read += 1;
    if (read > READABLE_VALUES) return undefined;
    if (Cause.isCause(at)) {
      for (const reason of at.reasons) {
        if (Cause.isDieReason(reason)) pending.push(reason.defect);
        else if (Cause.isFailReason(reason)) pending.push(reason.error);
      }
      continue;
    }
    values.push(at);
    pending.push(Option.getOrUndefined(decodeNestedCause(at))?.cause);
  }
  return values;
}

/** Whether `cause` may hold a failed query: one is in it, or it is too deep to tell. */
export function mayCarryQuery(cause: unknown): boolean {
  const values = causeValues(cause);
  return values === undefined || values.some((at) => {
    const tagged = decodeTaggedFailure(at);
    return Option.isSome(tagged) && QUERY_FAILURES.has(tagged.value._tag) || Option.isSome(decodeFailedQuery(at));
  });
}

/**
 * `value` by its tags, SQLSTATE and any failed query's statement, or else its error class: never a message or field,
 * since a query's message holds its params and the database's can quote the values it refused. `unreadable` when
 * reading it throws.
 */
export function safeSummary(cause: unknown): string {
  try {
    const name = Option.getOrUndefined(decodeErrorName(cause))?.name;
    const kind = typedFailureSummary(cause, name !== undefined && IDENTIFIER.test(name) ? name : "defect");
    const query = causeValues(cause)
      ?.map((at) => Option.getOrUndefined(decodeFailedQuery(at))?.query)
      .find((at) => at !== undefined);
    const statement = query === undefined ? undefined : statementName(query);
    return statement === undefined ? kind : `${kind} in ${statement}`;
  } catch {
    // A getter that throws hides what the value is; it is named as nothing more.
    return "unreadable";
  }
}

const READABLE_FIELDS = 1024;

function isObject(cause: unknown): cause is object {
  return cause !== null && cause !== undefined && Object(cause) === cause;
}

/**
 * Whether a failed query may be anywhere in `cause`: in a Cause, a `cause` chain, any own field, symbol-keyed or not,
 * or a Map or Set entry. A value it cannot read whole counts as holding one: more than [`READABLE_FIELDS`] values, a
 * getter other than an error's `stack`, or anything that throws. Getters are never called.
 */
export function holdsQuery(cause: unknown): boolean {
  const seen = new Set<object>();
  const pending: unknown[] = [cause];
  const read = (cause: unknown) => {
    pending.push(cause);
    return pending.length <= READABLE_FIELDS;
  };
  try {
    for (let at = 0; at < pending.length; at += 1) {
      const value = pending[at];
      if (!isObject(value) || seen.has(value)) continue;
      seen.add(value);
      if (mayCarryQuery(value)) return true;
      for (const key of Reflect.ownKeys(value)) {
        const field = Reflect.getOwnPropertyDescriptor(value, key);
        if (field?.get !== undefined && !(key === "stack" && value instanceof Error)) return true;
        if (!read(field?.value)) return true;
      }
      if (value instanceof Map) {
        for (const [key, entry] of value) if (!read(key) || !read(entry)) return true;
      } else if (value instanceof Set) {
        for (const entry of value) if (!read(entry)) return true;
      }
    }
  } catch {
    return true;
  }
  return false;
}

/**
 * What a log may hold of `cause`: all of it, unless a failed query may be in it. Then every reason is named by
 * [`safeSummary`], and an interruption as such.
 */
export function loggableCause(cause: Cause.Cause<unknown>): Cause.Cause<unknown> | string {
  if (!holdsQuery(cause)) return cause;
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

// A logged record is whatever its caller passed, so its fields are too.
// oxlint-disable-next-line anti-slop/no-object-parameters, anti-slop/no-unsafe-dictionary-type
function redactFields(record: object, depth: number): Record<string, unknown> {
  return Object.fromEntries(Object.keys(record).map((key) => {
    const field = Reflect.getOwnPropertyDescriptor(record, key);
    return [key, field?.get === undefined ? redact(field?.value, depth + 1) : "[redacted]"];
  }));
}

function redactedCause(cause: Cause.Cause<unknown>): Cause.Cause<unknown> {
  try {
    const loggable = loggableCause(cause);
    return Cause.isCause(loggable) ? loggable : Cause.die(loggable);
  } catch {
    // Reading the cause threw, so nothing in it can be shown as safe.
    return Cause.die("[redacted]");
  }
}

// Log annotations are whatever their callers set, so their fields are too.
// oxlint-disable-next-line anti-slop/no-unsafe-dictionary-type
function redactedAnnotations(annotations: Readonly<Record<string, unknown>>): ReturnType<typeof redactFields> {
  try {
    return redactFields(annotations, 0);
  } catch {
    // Reading the annotations threw, so nothing in them can be shown as safe.
    return { annotations: "[redacted]" };
  }
}

/**
 * `fiber` as a logger reads it, with its log annotations redacted: the default loggers print them beside the message.
 * The fiber itself when none may hold a failed query.
 */
function redactedFiber(fiber: Fiber.Fiber<unknown, unknown>): Fiber.Fiber<unknown, unknown> {
  const annotations = fiber.getRef(References.CurrentLogAnnotations);
  if (!holdsQuery(annotations)) return fiber;
  const context = Context.add(fiber.context, References.CurrentLogAnnotations, redactedAnnotations(annotations));
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

export const redactedLoggers: ReadonlySet<Logger.Logger<unknown, unknown>> = new Set([
  redactingLogger([Logger.defaultLogger, Logger.tracerLogger]),
]);

export const RedactedLogsLive = Layer.succeed(Logger.CurrentLoggers, redactedLoggers);

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

export function redactSpanExits<Provided, E, R>(tracing: Layer.Layer<Provided, E, R>): Layer.Layer<Provided, E, R> {
  return Layer.effect(Tracer.Tracer, Effect.map(Effect.service(Tracer.Tracer), redactingTracer)).pipe(Layer.provideMerge(tracing));
}
