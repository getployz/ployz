import { Effect, Fiber } from "effect";
import { TestClock } from "effect/testing";
import { expect, it } from "vitest";
import type { ServiceUptime } from "./uptime-status";
import { whileChecking } from "./uptime.commands.server";

const uptime = (answered: boolean): ServiceUptime => ({
  check: { id: "check", url: "https://web.example", intervalSeconds: 60 },
  status: "up", uptime: 1, points: [], checks: [], latest: [], incidents: [], regions: {},
  probe: { requestedAt: "2026-10-12T00:00:00.000Z", answered },
});

/** Runs `whileChecking` over reads that answer on the `answersOn`th, with a minute of test time to wait in. */
const wait = (answersOn: number) => Effect.runPromise(Effect.gen(function* () {
  let reads = 0;
  const read = Effect.sync(() => uptime(++reads >= answersOn));
  const fiber = yield* Effect.forkChild(whileChecking(read));
  yield* TestClock.adjust("1 minute");
  const last = yield* Fiber.join(fiber);
  return { reads, answered: last?.probe?.answered };
}).pipe(Effect.provide(TestClock.layer())));

it("stops waiting once the checker answers, after one more read for the round's results", async () => {
  expect(await wait(3)).toEqual({ reads: 4, answered: true });
});

it("gives up on a round the checker never answers within 10 s, so the CLI's 15 s request still gets an answer", async () => {
  const { reads, answered } = await wait(Infinity);
  expect(answered).toBe(false);
  expect(reads).toBeGreaterThanOrEqual(9);
  expect(reads).toBeLessThanOrEqual(11);
});
