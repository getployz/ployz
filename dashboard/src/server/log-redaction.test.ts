import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { Cause, Effect, Logger, Tracer } from "effect";
import { SqlError, UnknownError } from "effect/unstable/sql/SqlError";
import { inspect } from "node:util";
import { describe, expect, it, vi } from "vitest";
import { RedactedLogsLive, redactingLogger } from "#/server/log-redaction";

const failedQuery = () => new EffectDrizzleQueryError({
  query: "insert into machine_enrollment_token (token_hash) values ($1)",
  params: ["secret-param"],
  cause: Cause.fail(new SqlError({ reason: new UnknownError({ cause: { code: "P0001", message: "secret-message" } }) })),
});

const SUMMARY = "EffectDrizzleQueryError > SqlError > UnknownError (P0001) in insert machine_enrollment_token";

function logged(program: Effect.Effect<void>) {
  const entries: { message: unknown; cause: Cause.Cause<unknown> }[] = [];
  const capture = Logger.make(({ message, cause }) => {
    entries.push({ message, cause });
  });
  Effect.runSync(program.pipe(Effect.provideService(Logger.CurrentLoggers, new Set([redactingLogger(capture)]))));
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

  it("keeps a failed query out of the console and the span through the default loggers", () => {
    const lines: string[] = [];
    const spies = (["log", "error", "warn", "info", "debug"] as const)
      .map((level) => vi.spyOn(console, level).mockImplementation((...args: unknown[]) => {
        lines.push(args.map((arg) => Cause.isCause(arg) ? Cause.pretty(arg) : inspect(arg, { depth: 8 })).join(" "));
      }));
    let events = "";
    try {
      Effect.runSync(Effect.gen(function* () {
        yield* Effect.logError("Runtime frame not recorded", { cause: failedQuery() });
        const span = yield* Effect.currentSpan;
        events = inspect(span instanceof Tracer.NativeSpan ? span.events : span, { depth: 8 });
      }).pipe(Effect.withSpan("probe"), Effect.provide(RedactedLogsLive)));
    } finally {
      for (const spy of spies) spy.mockRestore();
    }

    const text = `${lines.join("\n")}\n${events}`;
    expect(lines.join("\n")).toContain("Runtime frame not recorded");
    expect(events).toContain("Runtime frame not recorded");
    expect(text).toContain(SUMMARY);
    expect(text).not.toMatch(/secret-/u);
  });
});
