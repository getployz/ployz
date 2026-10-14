import { Data, Effect, Semaphore } from "effect";
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { whileLocked } from "#/server/advisory-lock.server";
import { type PostgresTestHarness, startPostgresTestHarness } from "#/test/postgres";

class LockFailed extends Data.TaggedError("LockFailed") {}

describe("whileLocked", () => {
  let harness: PostgresTestHarness;
  beforeAll(async () => { harness = await startPostgresTestHarness(); }, 60_000);
  afterAll(async () => { await harness?.stop(); });

  it("fails with the lock's error, so a caller can answer it, and never runs the body", async () => {
    let ran = false;
    const error = await harness.runEffect(
      whileLocked(Semaphore.makeUnsafe(1), Effect.fail(new LockFailed()), Effect.sync(() => { ran = true; })).pipe(Effect.flip),
    );
    expect(error).toBeInstanceOf(LockFailed);
    expect(ran).toBe(false);
  });
});
