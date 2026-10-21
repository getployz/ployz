import { RuleTester } from "oxlint/plugins-dev";
import { describe, it } from "vitest";

import { noRealTimeWaitInTestsRule } from "./no-real-time-wait-in-tests.ts";

RuleTester.describe = describe;
RuleTester.it = it;

const filename = "feature.test.ts";
const tester = new RuleTester();

tester.run("no-real-time-wait-in-tests", noRealTimeWaitInTestsRule, {
  valid: [
    {
      filename,
      code: `it.effect("expires", () => Effect.gen(function* () {
        const fiber = yield* Effect.fork(Effect.sleep("5 seconds"));
        yield* TestClock.adjust("5 seconds");
        yield* Fiber.join(fiber);
      }));`,
    },
    {
      filename,
      code: `const pause = () => Effect.sleep("1 second");
      it.effect("drives a shared helper", () => Effect.gen(function* () {
        yield* Effect.fork(pause());
        yield* TestClock.adjust("1 second");
      }));`,
    },
    { filename, code: `it("names a wait", () => expect("Effect.sleep(1000)").toBeTypeOf("string"));` },
    { filename, code: `it("schedules a callback", () => { setTimeout(() => done(), 0); });` },
    { filename, code: `it("polls", async () => { await vi.waitFor(() => expect(rows()).toHaveLength(1)); });` },
    {
      filename,
      code: `it("debounces", async () => {
        vi.useFakeTimers();
        const fired = new Promise((resolve) => setTimeout(resolve, 1_000));
        await vi.advanceTimersByTimeAsync(1_000);
        await fired;
      });`,
    },
    {
      filename,
      code: `describe("debounce", () => {
        beforeEach(() => vi.useFakeTimers({ toFake: ["setTimeout"] }));
        it("fires", async () => { await new Promise((resolve) => setTimeout(resolve, 1_000)); });
      });`,
    },
    { filename: "feature.ts", code: `export const pause = () => new Promise((resolve) => setTimeout(resolve, 10));` },
    {
      filename,
      code: `// oxlint-disable-next-line anti-slop/no-real-time-wait-in-tests -- the dev server exposes no readiness signal
      it("boots", async () => {});`,
    },
  ],
  invalid: [
    {
      filename,
      code: `it("waits on the global object", async () => { await new Promise((resolve) => globalThis.setTimeout(resolve, 300)); });`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it("restores real timers, then waits", async () => {
        vi.useFakeTimers();
        vi.useRealTimers();
        await new Promise((resolve) => setTimeout(resolve, 300));
      });`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it.effect("drives its own clock", () => Effect.gen(function* () {
        const fiber = yield* Effect.fork(Effect.sleep("5 seconds"));
        yield* TestClock.adjust("5 seconds");
        yield* Fiber.join(fiber);
      }));
      it.effect("never drives the clock", () => Effect.sleep("1 second"));`,
      errors: [{ messageId: "undrivenTestClock" }],
    },
    {
      filename,
      code: `it("waits", async () => { await new Promise((resolve) => setTimeout(resolve, 300)); });`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it.each([1, 2])("waits %i", async () => { await new Promise((resolve) => setTimeout(resolve, 10)); });`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it("races", async () => {
        await Promise.race([op(), new Promise((resolve) => setTimeout(() => resolve("held"), 2_000))]);
      });`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it("waits after the fake clock is gone", async () => {
        vi.useFakeTimers({ toFake: ["Date"] });
        await new Promise((resolve) => setTimeout(resolve, 10));
      });`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it("advances with the wall clock", async () => {
        vi.useFakeTimers({ shouldAdvanceTime: true });
        await new Promise((resolve) => setTimeout(resolve, 10));
      });`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it("fakes too late", async () => {
        await new Promise((resolve) => setTimeout(resolve, 10));
        vi.useFakeTimers();
      });`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it.live("sleeps", () => Effect.sleep("300 millis"));`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it.live.skipIf(ci)("retries", () => op.pipe(Effect.retry({ schedule: Schedule.spaced("10 millis") })));`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it("delays", () => Effect.runPromise(op.pipe(Effect.delay("1 second"))));`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `layer(Live, { excludeTestServices: true })("live", (it) => {
        it.effect("sleeps", () => Effect.sleep("1 second"));
      });
      TestClock.adjust("1 second");`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `it.effect("hangs", () => Effect.sleep("1 second"));`,
      errors: [{ messageId: "undrivenTestClock" }],
    },
    {
      filename,
      code: `// TestClock.adjust is only named here
      it.effect("hangs", () => Effect.sleep("1 second"));`,
      errors: [{ messageId: "undrivenTestClock" }],
    },
    {
      filename,
      code: `it.effect("flushes", () => Effect.promise(() => new Promise((resolve) => setTimeout(resolve, 0))));`,
      errors: [{ messageId: "mixedClock" }],
    },
    {
      filename,
      code: `const pause = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));
      const settle = Effect.sleep("1 second");
      it("uses helpers", async () => { await pause(10); });`,
      errors: [{ messageId: "realClock" }, { messageId: "realClock" }],
    },
    {
      filename,
      code: `const poll = Effect.gen(function* () { while (!(yield* ready)) yield* Effect.sleep("20 millis"); });`,
      errors: [{ messageId: "realClock" }],
    },
    {
      filename,
      code: `import { setTimeout as sleep } from "node:timers/promises";
      import * as timers from "timers/promises";
      it("sleeps", async () => { await sleep(600); await timers.setTimeout(10); });`,
      errors: [{ messageId: "realClock" }, { messageId: "realClock" }],
    },
    {
      filename,
      code: `// oxlint-disable-next-line anti-slop/no-real-time-wait-in-tests
      it("waits", async () => { await new Promise((resolve) => setTimeout(resolve, 300)); });`,
      errors: [{ messageId: "unexplainedSuppression" }, { messageId: "realClock" }],
    },
  ],
});
