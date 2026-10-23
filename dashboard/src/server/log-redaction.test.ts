import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { Cause, Data, Effect, Exit, Layer, Logger, References, Tracer } from "effect";
import { SqlError, UnknownError } from "effect/unstable/sql/SqlError";
import { inspect } from "node:util";
import { describe, expect, it, vi } from "vitest";
import { RedactedLogsLive, redactingLogger, redactSpanExits } from "#/server/log-redaction";
import { encodePublicBoundaryError, loggableCause } from "#/server/public-error";

const failedQuery = () => new EffectDrizzleQueryError({
  query: "insert into machine_enrollment_token (token_hash) values ($1)",
  params: ["secret-param"],
  cause: Cause.fail(new SqlError({ reason: new UnknownError({ cause: { code: "P0001", message: "secret-message" } }) })),
});

class Wrapped extends Data.TaggedError("Wrapped")<{ readonly error: unknown }> {}

const SUMMARY = "EffectDrizzleQueryError > SqlError > UnknownError (P0001) in insert machine_enrollment_token";

function logged(program: Effect.Effect<void>) {
  const entries: { message: unknown; cause: Cause.Cause<unknown> }[] = [];
  const capture = Logger.make(({ message, cause }) => {
    entries.push({ message, cause });
  });
  Effect.runSync(program.pipe(Effect.provideService(Logger.CurrentLoggers, new Set([redactingLogger([capture])]))));
  return entries.map(({ message, cause }) => `${inspect(message, { depth: 8 })} ${Cause.pretty(cause)}`).join("\n");
}

describe("redacted logs", () => {
  it("logs a failed query in a context field by its summary, and keeps the other fields", () => {
    const text = logged(Effect.logWarning("A Config Store sweep failed.", { organizationId: "org_1", error: failedQuery() }));

    expect(text).toContain("A Config Store sweep failed.");
    expect(text).toContain("org_1");
    expect(text).toContain(SUMMARY);
    expect(text).not.toMatch(/secret-/u);
  });

  it("logs a caught Cause that died on a query by its summary", () => {
    const text = logged(Effect.logWarning("An outbox event waits for the sweep.", Cause.die(failedQuery())));

    expect(text).toContain(SUMMARY);
    expect(text).not.toMatch(/secret-/u);
  });

  it("logs a failure that carries no query whole", () => {
    expect(logged(Effect.logWarning("GitHub did not complete a connect.", new Error("plain detail")))).toContain("plain detail");
  });

  it.each([
    ["a record past the depth it reads", () => ({ a: { b: { c: { error: failedQuery() } } } })],
    ["an array past the depth it reads", () => ({ a: { b: { c: [failedQuery()] } } })],
    ["a record holding one in a field other than its cause", () => ({ a: { b: { error: failedQuery() } } })],
    ["an error holding one in a field other than its cause", () => new Wrapped({ error: failedQuery() })],
    ["a Map", () => new Map([["k", failedQuery()]])],
    ["a Set", () => new Set([failedQuery()])],
    ["an AggregateError", () => new AggregateError([failedQuery()])],
    ["a query error spread into a record", () => ({ ...failedQuery() })],
    ["a cyclic record", () => {
      const record = { error: failedQuery(), self: {} };
      record.self = record;
      return record;
    }],
    ["a Cause holding one in a defect's field", () => Cause.die(new Wrapped({ error: failedQuery() }))],
  ])("keeps a failed query in %s out of the log", (_holder, value) => {
    const text = logged(Effect.logWarning("A sweep failed.", value()));

    expect(text).toContain("A sweep failed.");
    expect(text).not.toMatch(/secret-/u);
  });

  it.each([
    ["a symbol-keyed field", () => ({ [Symbol("error")]: failedQuery() })],
    ["a Map's own field", () => Object.assign(new Map(), { error: failedQuery() })],
    ["a getter", () => ({ get error() { return failedQuery(); } })],
  ])("keeps a failed query behind %s out of the log", (_holder, value) => {
    expect(logged(Effect.logWarning("A sweep failed.", value()))).not.toMatch(/secret-/u);
  });

  it.each([
    ["a cause getter that throws", () => ({ get cause(): never { throw new Error("unreadable"); } })],
    ["a Proxy whose keys throw", () => new Proxy({}, { ownKeys: () => { throw new Error("unreadable"); } })],
    ["a failed query beside a getter that throws", () => ({ error: failedQuery(), get detail(): never { throw new Error("unreadable"); } })],
  ])("logs %s without failing", (_holder, value) => {
    const text = logged(Effect.logWarning("A sweep failed.", value()));

    expect(text).toContain("A sweep failed.");
    expect(text).not.toMatch(/secret-/u);
  });

  it("logs a large Map that holds no query without failing", () => {
    const entries = new Map(Array.from({ length: 10_000 }, (_, at) => [`k${at}`, at]));

    expect(logged(Effect.logWarning("Many entries.", entries))).toContain("Many entries.");
  });

  it("names a failed query in any field of a public boundary's cause", () => {
    const loggable = loggableCause(Cause.die(new Error("wrapper", { cause: { error: failedQuery() } })));

    expect(Cause.isCause(loggable) ? Cause.pretty(loggable) : loggable).not.toMatch(/secret-/u);
  });

  it("encodes a public boundary's defect whose code getter throws", () => {
    const error = new Error("wrapper");
    Object.defineProperty(error, "code", { get: (): never => { throw new Error("unreadable"); } });
    const reported: unknown[] = [];

    encodePublicBoundaryError(Cause.die(error), { report: (_kind, cause) => reported.push(cause) });

    expect(reported).toEqual(["unreadable"]);
  });

  it("logs with annotations it cannot read", () => {
    const annotations = new Proxy({}, { ownKeys: () => { throw new Error("unreadable"); } });

    const text = logged(Effect.logWarning("A sweep failed.").pipe(Effect.provideService(References.CurrentLogAnnotations, annotations)));

    expect(text).toContain("A sweep failed.");
  });

  it("logs an undefined value", () => {
    expect(logged(Effect.logWarning("Nothing to add.", undefined, { error: undefined }))).toContain("Nothing to add.");
  });

  it("keeps a failed query out of the console and the span through the default loggers", () => {
    const lines: string[] = [];
    const spies = (["log", "error", "warn", "info", "debug"] as const)
      .map((level) => vi.spyOn(console, level).mockImplementation((...args: unknown[]) => {
        lines.push(args.map((arg) => Cause.isCause(arg) ? Cause.pretty(arg) : inspect(arg, { depth: 8 })).join(" "));
      }));
    let events: Tracer.NativeSpan["events"] = [];
    try {
      Effect.runSync(Effect.gen(function* () {
        yield* Effect.logError("Runtime frame not recorded", { cause: failedQuery() });
        yield* Effect.logWarning("Runtime frame not recorded", Cause.die(failedQuery()));
        const span = yield* Effect.currentSpan;
        if (span instanceof Tracer.NativeSpan) events = span.events;
      }).pipe(Effect.withSpan("probe"), Effect.provide(RedactedLogsLive)));
    } finally {
      for (const spy of spies) spy.mockRestore();
    }

    const printed = lines.join("\n");
    expect(printed).toContain("Runtime frame not recorded");
    expect(printed).toContain(SUMMARY);
    expect(printed).not.toMatch(/secret-/u);
    expect(events[1]?.[0]).toBe("Runtime frame not recorded");
    const recorded = inspect(events, { depth: 8 });
    expect(recorded).toContain(SUMMARY);
    expect(recorded).not.toMatch(/secret-/u);
    expect(events[1]?.[2]?.["effect.cause"]).toContain(SUMMARY);
  });

  it("keeps a failed query in a log annotation out of the console and the span, and keeps the others", () => {
    const lines: string[] = [];
    const spies = (["log", "error", "warn", "info", "debug"] as const)
      .map((level) => vi.spyOn(console, level).mockImplementation((...args: unknown[]) => {
        lines.push(args.map((arg) => inspect(arg, { depth: 8 })).join(" "));
      }));
    let events: Tracer.NativeSpan["events"] = [];
    try {
      Effect.runSync(Effect.gen(function* () {
        yield* Effect.logWarning("A sweep failed.").pipe(Effect.annotateLogs({ organizationId: "org_1", error: failedQuery() }));
        const span = yield* Effect.currentSpan;
        if (span instanceof Tracer.NativeSpan) events = span.events;
      }).pipe(Effect.withSpan("probe"), Effect.provide(RedactedLogsLive)));
    } finally {
      for (const spy of spies) spy.mockRestore();
    }

    const printed = lines.join("\n");
    const recorded = inspect(events, { depth: 8 });
    expect(printed).toContain("org_1");
    expect(printed).toContain(SUMMARY);
    expect(printed).not.toMatch(/secret-/u);
    expect(recorded).toContain("org_1");
    expect(recorded).not.toMatch(/secret-/u);
  });

  it("ends a span that failed on a query with its summary, never its params", () => {
    const spans: Tracer.NativeSpan[] = [];
    const tracing = Layer.succeed(Tracer.Tracer, Tracer.make({
      span(options) {
        const span = new Tracer.NativeSpan(options);
        spans.push(span);
        return span;
      },
    }));

    Effect.runSyncExit(Effect.die(failedQuery()).pipe(Effect.withSpan("recordNames"), Effect.provide(redactSpanExits(tracing))));

    const status = spans[0]?.status;
    const exit = status?._tag === "Ended" ? status.exit : Exit.void;
    const text = Exit.match(exit, { onFailure: Cause.pretty, onSuccess: () => "ended without failing" });
    expect(text).toContain(SUMMARY);
    expect(text).not.toMatch(/secret-/u);
  });
});
