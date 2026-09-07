import { Cause, Context, Data, Effect, Layer, ManagedRuntime, Schema } from "effect";
import { NonRetriableError } from "inngest";
import { describe, expect, it } from "vitest";
import {
  isNonRetriableInngestCause,
  makeEffectRunner,
  makeInngestEffectRunner,
} from "#/server/run.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";

class Resource extends Context.Service<Resource, { readonly id: string }>()(
  "test/Resource",
) {}

class ExpectedFailure extends Data.TaggedError("ExpectedFailure") {}

class RetriableProviderFailure extends Data.TaggedError("RetriableProviderFailure")<{
  readonly retriable: true;
}> {}

class TerminalProviderFailure extends Data.TaggedError("TerminalProviderFailure")<{
  readonly retriable: false;
  readonly failureCode: string;
}> {}

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
            failureCode: "deploy_image_not_pullable",
          }),
        ),
      ),
    ).rejects.toMatchObject({
      name: "NonRetriableError",
      cause: expect.objectContaining({
        failureCode: "deploy_image_not_pullable",
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
});
