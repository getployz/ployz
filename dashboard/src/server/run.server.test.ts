import { Cause, Context, Data, Effect, Layer, ManagedRuntime } from "effect";
import { EffectDrizzleQueryError } from "drizzle-orm/effect-core";
import { SqlError, UnknownError } from "effect/unstable/sql/SqlError";
import { describe, expect, it, vi } from "vitest";
import { makeEffectRunner } from "#/server/run.server";

class Resource extends Context.Service<Resource, { readonly id: string }>()(
  "test/Resource",
) {}

class ExpectedFailure extends Data.TaggedError("ExpectedFailure") {}


const failedQuery = () => new EffectDrizzleQueryError({
  query: "insert into machine_enrollment_token (token_hash) values ($1)",
  params: ["secret-param"],
  cause: Cause.fail(new SqlError({ reason: new UnknownError({ cause: { code: "P0001", message: "secret-message" } }) })),
});

describe("Effect execution boundary", () => {
  it("reuses one managed scope and releases it on dispose", async () => {
    let acquired = 0;
    let released = 0;
    const layer = Layer.effect(
      Resource,
      Effect.acquireRelease(
        Effect.sync(() => {
          acquired += 1;
          return { id: crypto.randomUUID() };
        }),
        () => Effect.sync(() => {
          released += 1;
        }),
      ),
    );
    const runtime = ManagedRuntime.make(layer);
    const run = makeEffectRunner(runtime);

    const resourceId = Effect.gen(function* () {
      return (yield* Resource).id;
    });
    const first = await run(resourceId);
    const second = await run(resourceId);
    expect(second).toBe(first);
    expect(acquired).toBe(1);

    await runtime.dispose();
    expect(released).toBe(1);
  });

  it("links AbortSignal cancellation to Effect interruption", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const run = makeEffectRunner(runtime);
    const controller = new AbortController();
    let interrupted = false;
    const pending = run(
      Effect.never.pipe(
        Effect.onInterrupt(() => Effect.sync(() => {
          interrupted = true;
        })),
      ),
      { signal: controller.signal },
    );

    controller.abort();
    await expect(pending).rejects.toSatisfy(
      (cause: unknown) => Cause.isCause(cause) && Cause.hasInterrupts(cause),
    );
    expect(interrupted).toBe(true);
    await runtime.dispose();
  });

  it("preserves the full Effect cause for defects", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const run = makeEffectRunner(runtime);

    await expect(run(Effect.die("provider-secret"))).rejects.toSatisfy(
      (cause: unknown) => Cause.isCause(cause) && Cause.hasDies(cause),
    );
    await runtime.dispose();
  });

  it("throws the typed failure when a defect is raised alongside it", async () => {
    const runtime = ManagedRuntime.make(Layer.empty);
    const run = makeEffectRunner(runtime);

    const program = Effect.fail(new ExpectedFailure()).pipe(
      Effect.ensuring(Effect.die("finalizer-defect")),
    );
    await expect(run(program)).rejects.toBeInstanceOf(ExpectedFailure);
    await runtime.dispose();
  });

  it("logs a query that died under a typed failure without its params or message", async () => {
    const lines: string[] = [];
    const capture = (...args: unknown[]) => {
      lines.push(args.map((arg) => Cause.isCause(arg) ? Cause.pretty(arg) : String(JSON.stringify(arg) ?? arg)).join(" "));
    };
    const spies = (["log", "error", "warn", "info", "debug"] as const).map((level) => vi.spyOn(console, level).mockImplementation(capture));
    const runtime = ManagedRuntime.make(Layer.empty);
    const run = makeEffectRunner(runtime);

    try {
      await expect(run(Effect.fail(new ExpectedFailure()).pipe(Effect.ensuring(Effect.die(failedQuery())))))
        .rejects.toBeInstanceOf(ExpectedFailure);
      await vi.waitFor(() => expect(lines.join("\n")).toContain("Defect shadowed by a typed failure."));
    } finally {
      for (const spy of spies) spy.mockRestore();
      await runtime.dispose();
    }

    const logged = lines.join("\n");
    expect(logged).toContain("Defect shadowed by a typed failure.");
    expect(logged).toContain("insert machine_enrollment_token");
    expect(logged).not.toContain("secret-param");
    expect(logged).not.toContain("secret-message");
  });
});
