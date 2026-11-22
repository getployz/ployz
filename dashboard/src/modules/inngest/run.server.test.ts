import { Cause, Data, Effect, Layer, ManagedRuntime, Schema } from "effect";
import { NonRetriableError } from "inngest";
import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { SqlError, UnknownError } from "effect/unstable/sql/SqlError";
import { describe, expect, it } from "vitest";
import { isNonRetriableInngestCause, makeInngestEffectRunner } from "#/modules/inngest/run.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { makeEffectRunner } from "#/server/run.server";

class ExpectedFailure extends Data.TaggedError("ExpectedFailure") {}

class RetriableProviderFailure extends Data.TaggedError("RetriableProviderFailure")<{
  readonly retriable: true;
}> {}

class TerminalProviderFailure extends Data.TaggedError("TerminalProviderFailure")<{
  readonly retriable: false;
  readonly failureCode: string;
}> {}


const failedQuery = () => new EffectDrizzleQueryError({
  query: "insert into machine_enrollment_token (token_hash) values ($1)",
  params: ["secret-param"],
  cause: Cause.fail(new SqlError({ reason: new UnknownError({ cause: { code: "P0001", message: "secret-message" } }) })),
});

describe("Inngest effect boundary", () => {
  it("names a query that died at the Inngest boundary without its params", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const runInngest = makeInngestEffectRunner(makeEffectRunner(runtime));

    await expect(runInngest(Effect.die(failedQuery()))).rejects.toSatisfy(
      (cause: unknown) =>
        cause instanceof Error &&
        cause.message === "EffectDrizzleQueryError > SqlError > UnknownError (P0001) in insert machine_enrollment_token" &&
        cause.cause === undefined,
    );
    await runtime.dispose();
  });

  it("keeps a died NonRetriableError non-retriable but drops the query it wraps", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const runInngest = makeInngestEffectRunner(makeEffectRunner(runtime));

    await expect(runInngest(Effect.die(new NonRetriableError("wrapper", { cause: failedQuery() })))).rejects.toSatisfy(
      (cause: unknown) =>
        cause instanceof NonRetriableError && cause.cause === undefined && !cause.message.includes("secret-param"),
    );
    await runtime.dispose();
  });

  it("keeps the defect message and full cause at the Inngest boundary", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const runInngest = makeInngestEffectRunner(makeEffectRunner(runtime));

    await expect(
      runInngest(Effect.die(new Error("Insert returned no row."))),
    ).rejects.toSatisfy(
      (cause: unknown) =>
        cause instanceof Error &&
        !(cause instanceof NonRetriableError) &&
        cause.message === "Insert returned no row." &&
        Cause.isCause(cause.cause) &&
        Cause.hasDies(cause.cause),
    );
    await expect(
      runInngest(Effect.die("string-defect")),
    ).rejects.toSatisfy(
      (cause: unknown) =>
        cause instanceof Error &&
        cause.message.includes("string-defect") &&
        Cause.isCause(cause.cause),
    );
    await runtime.dispose();
  });

  it("classifies a typed failure as non-retriable even when a defect shadows it", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const runInngest = makeInngestEffectRunner(makeEffectRunner(runtime));

    const program = Effect.fail(new Conflict({ message: "already in progress" })).pipe(
      Effect.ensuring(Effect.die("finalizer-defect")),
    );
    await expect(runInngest(program)).rejects.toMatchObject({
      name: "NonRetriableError",
      message: "already in progress",
      cause: expect.any(Conflict),
    });
    await runtime.dispose();
  });

  it("rethrows a NonRetriableError raised as a defect without wrapping it", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const runInngest = makeInngestEffectRunner(makeEffectRunner(runtime));
    const terminal = new NonRetriableError("do not retry");

    await expect(runInngest(Effect.die(terminal))).rejects.toBe(terminal);
    expect(isNonRetriableInngestCause(Cause.die(terminal))).toBe(true);
    await runtime.dispose();
  });

  it("forwards non-Error failures as the NonRetriableError cause", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const runInngest = makeInngestEffectRunner(makeEffectRunner(runtime));

    await expect(
      runInngest(Effect.fail({ retriable: false, failureCode: "plain_object" } as never)),
    ).rejects.toMatchObject({
      name: "NonRetriableError",
      cause: { retriable: false, failureCode: "plain_object" },
    });
    await runtime.dispose();
  });

  it("classifies schema, retriable, and public-category failures for Inngest", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const runInngest = makeInngestEffectRunner(makeEffectRunner(runtime));
    const idSchema = Schema.Struct({ id: Schema.String });

    await expect(
      runInngest(Schema.decodeUnknownEffect(idSchema)({})),
    ).rejects.toBeInstanceOf(NonRetriableError);
    await expect(
      runInngest(
        Effect.fail(
          new TerminalProviderFailure({
            retriable: false,
            failureCode: "sdk_preparation_failed",
          }),
        ),
      ),
    ).rejects.toMatchObject({
      name: "NonRetriableError",
      cause: expect.objectContaining({
        failureCode: "sdk_preparation_failed",
      }),
    });
    await expect(
      runInngest(Effect.fail(new Conflict({ message: "already in progress" }))),
    ).rejects.toBeInstanceOf(NonRetriableError);
    await expect(
      runInngest(Effect.fail(new Validation({ message: "invalid" }))),
    ).rejects.toBeInstanceOf(NonRetriableError);
    await expect(
      runInngest(Effect.fail(new NotFound({ message: "missing" }))),
    ).rejects.toBeInstanceOf(NonRetriableError);

    await expect(
      runInngest(Effect.fail(new ExpectedFailure())),
    ).rejects.toBeInstanceOf(ExpectedFailure);
    await expect(
      runInngest(Effect.fail(new RetriableProviderFailure({ retriable: true }))),
    ).rejects.toBeInstanceOf(RetriableProviderFailure);

    await runtime.dispose();
  });
});

describe("isNonRetriableInngestCause", () => {
  it("matches schema errors, retriable: false, and conflict/validation/not-found", () => {
    expect(isNonRetriableInngestCause(new ExpectedFailure())).toBe(false);
    expect(
      isNonRetriableInngestCause(new RetriableProviderFailure({ retriable: true })),
    ).toBe(false);
    expect(
      isNonRetriableInngestCause(
        new TerminalProviderFailure({
          retriable: false,
          failureCode: "sdk_surface_not_shipped",
        }),
      ),
    ).toBe(true);
    expect(
      isNonRetriableInngestCause(new Conflict({ message: "conflict" })),
    ).toBe(true);
    expect(
      isNonRetriableInngestCause(new Validation({ message: "invalid" })),
    ).toBe(true);
    expect(
      isNonRetriableInngestCause(new NotFound({ message: "missing" })),
    ).toBe(true);
  });

  it("classifies a raw Cause by its squashed failure", () => {
    expect(
      isNonRetriableInngestCause(Cause.fail(new Conflict({ message: "conflict" }))),
    ).toBe(true);
    expect(
      isNonRetriableInngestCause(
        Cause.die(new TerminalProviderFailure({ retriable: false, failureCode: "x" })),
      ),
    ).toBe(true);
    expect(isNonRetriableInngestCause(Cause.die("boom"))).toBe(false);
    expect(isNonRetriableInngestCause(Cause.fail(new ExpectedFailure()))).toBe(false);
  });
});
