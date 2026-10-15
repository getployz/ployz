import { InngestTestEngine } from "@inngest/test";
import { Effect } from "effect";
import { type Context, Inngest } from "inngest";
import { describe, expect, it, vi } from "vitest";
import { useServiceFreeEffectRunner } from "#/test/service-free-effect-runner";
import * as rollup from "./rollup";
import { createWebAnalyticsFunctions } from "./web-analytics.inngest";

useServiceFreeEffectRunner();

const ran: string[] = [];
/** Every site's step rolls up one day, except site B's, which always fails. */
const site = (environmentId: string) =>
  environmentId === "env-b" ? Effect.die(new Error("site b is broken")) : Effect.sync(() => ran.push(environmentId)).pipe(Effect.as(1));
vi.spyOn(rollup, "rollupSites").mockImplementation(() => Effect.succeed(["env-a", "env-b", "env-c"]));
vi.spyOn(rollup, "closeDays").mockImplementation(site);
vi.spyOn(rollup, "rebuildDirtyDays").mockImplementation(site);

/** A step that has spent its retries: `step.run` rejects in the function with the step's last error, as Inngest's does. */
const finalAttempt = (ctx: Context.Any): Context.Any => {
  // SAFETY: These steps return JSON-safe numbers, so the result needs no Jsonify step.
  const run = (async (_id: string, fn: () => Promise<number>) => fn()) as typeof ctx.step.run;
  return { ...ctx, step: { ...ctx.step, run } };
};

describe("analytics cron functions", () => {
  it.each([0, 1])("a failing site leaves later sites to run, then fails the run naming it (function %i)", async (index) => {
    ran.length = 0;
    const fn = createWebAnalyticsFunctions(new Inngest({ id: "analytics-test" }))[index];
    if (fn === undefined) throw new Error("no function");
    const output = await new InngestTestEngine({ function: fn, transformCtx: finalAttempt }).execute();
    expect(ran).toEqual(["env-a", "env-c"]);
    expect(output.error).toMatchObject({ name: "NonRetriableError", message: "analytics steps failed for env-b" });
  });
});
