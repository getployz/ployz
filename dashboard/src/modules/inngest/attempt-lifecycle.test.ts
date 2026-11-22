import { InngestTestEngine } from "@inngest/test";
import { Effect, Option, Schema } from "effect";
import { Inngest } from "inngest";
import { describe, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { runInngestEffect } from "#/modules/inngest/run.server";
import { invokeOnFailure } from "#/test/inngest-failure";

const probe = attemptLifecycle({
  run: { id: "probe", triggers: [{ event: "probe/requested" }], handler: async () => null },
  cancelId: "cancel-probe",
  closeRun: () => Effect.succeed(1),
  sweep: { id: "probe-sweep", closeStale: () => Effect.succeed({}) },
});

describe("attemptLifecycle", () => {
  it("registers the run with its failure handler, a handler for Inngest's cancellation and a scheduled sweep", () => {
    const [run, cancel, sweep] = probe.createFunctions(new Inngest({ id: "test" }));

    expect(run.opts.onFailure).toEqual(expect.any(Function));
    expect(cancel.opts).toMatchObject({ triggers: [{ event: { name: "inngest/function.cancelled" } }] });
    expect(sweep.opts).toMatchObject({ triggers: [{ cron: expect.any(String) }] });
  });

  it("closes the run when Inngest cancels it, and skips another function's cancellation", async () => {
    const cancel = (cancelled: string) => new InngestTestEngine({
      function: probe.createCancel(new Inngest({ id: "ployz-cloud" })),
      events: [{ name: "inngest/function.cancelled", data: { function_id: cancelled, run_id: "run-1" } }],
      steps: [{ id: "close-run", handler: () => 1 }],
    }).execute();

    expect((await cancel("ployz-cloud-probe")).result).toEqual({ closed: 1 });
    expect((await cancel("ployz-cloud-other")).result).toEqual({ skipped: true });
  });

  it("gives the run its own retry count while the cancel and sweep functions keep 3", () => {
    const [run, cancel, sweep] = attemptLifecycle({
      run: { id: "probe", retries: 5, triggers: [{ event: "probe/requested" }], handler: async () => null },
      cancelId: "cancel-probe",
      closeRun: () => Effect.succeed(1),
      sweep: { id: "probe-sweep", closeStale: () => Effect.succeed({}) },
    }).createFunctions(new Inngest({ id: "test" }));

    expect([run.opts.retries, cancel.opts.retries, sweep.opts.retries]).toEqual([5, 3, 3]);
  });

  it("hands a failed run's error, and its triggering event only when the workflow asks for it, to closeRun", async () => {
    const closed: unknown[] = [];
    const probe = (triggeringOnFailure: boolean) => attemptLifecycle({
      run: { id: "probe", triggers: [{ event: "probe/requested" }], handler: async () => null },
      cancelId: "cancel-probe",
      triggeringEvent: Schema.Struct({ data: Schema.Struct({ attemptId: Schema.String }) }),
      triggeringOnFailure,
      closeRun: (runId, end, triggering, error) => {
        closed.push([runId, end, Option.getOrUndefined(triggering)?.data.attemptId, error?.message]);
        return Effect.succeed(1);
      },
      sweep: { id: "probe-sweep", closeStale: () => Effect.succeed({}) },
    }).createRun(new Inngest({ id: "test" }), asTestDouble<typeof runInngestEffect>()(async () => 1));
    const event = { name: "probe/requested", data: { attemptId: "a-1" } };
    await invokeOnFailure(probe(true), "run-1", event);
    await invokeOnFailure(probe(false), "run-2", event);
    expect(closed).toEqual([["run-1", "failure", "a-1", "the run failed"], ["run-2", "failure", undefined, "the run failed"]]);
  });

  it("names the cancelled function under the client's app id", async () => {
    const cancel = await new InngestTestEngine({
      function: probe.createCancel(new Inngest({ id: "self-hosted" })),
      events: [{ name: "inngest/function.cancelled", data: { function_id: "self-hosted-probe", run_id: "run-1" } }],
      steps: [{ id: "close-run", handler: () => 1 }],
    }).execute();

    expect(cancel.result).toEqual({ closed: 1 });
  });
});
