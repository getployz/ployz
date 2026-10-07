import { type Effect, Option, Schema } from "effect";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { inngestFunctionCancelledEnvelopeSchema, inngestFunctionCancelledEventType } from "#/modules/inngest/events";
import type { AppServices } from "#/server/runtime.server";
import { runInngestEffect } from "#/server/run.server";

type EffectRunner = typeof runInngestEffect;
type Activity<A> = Effect.Effect<A, Error, AppServices>;
type RunTrigger = Parameters<PloyzInngest["createFunction"]>[0]["triggers"];
type RunConcurrency = Parameters<PloyzInngest["createFunction"]>[0]["concurrency"];
type RunSingleton = Parameters<PloyzInngest["createFunction"]>[0]["singleton"];

export type RunEnd = "failure" | "cancellation";

const decodeFailedRun = Schema.decodeUnknownOption(Schema.Struct({ data: Schema.Struct({ run_id: Schema.String }) }));

export function attemptLifecycle<Result, More extends object, Triggering = never>(workflow: {
  readonly run: {
    readonly id: string;
    readonly triggers: RunTrigger;
    readonly concurrency?: RunConcurrency;
    readonly singleton?: RunSingleton;
    readonly handler: (
      ctx: { event: { data: unknown }; step: PloyzStepTools; runId: string },
      runEffect: EffectRunner,
    ) => Promise<Result>;
  };
  readonly cancelId: string;
  readonly triggeringEvent?: Schema.ConstraintDecoder<Triggering>;
  readonly closeRun: (runId: string, end: RunEnd, triggering: Option.Option<Triggering>) => Activity<number>;
  readonly sweep: {
    readonly id: string;
    readonly closeStale: () => Activity<Readonly<Record<string, number>>>;
    readonly afterSweep?: (step: PloyzStepTools, runEffect: EffectRunner) => Promise<More>;
  };
}) {
  const { run, closeRun, sweep, triggeringEvent } = workflow;
  const decodeTriggering = triggeringEvent === undefined ? () => Option.none() : Schema.decodeUnknownOption(triggeringEvent);
  const createRun = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
    inngest.createFunction(
      {
        id: run.id,
        retries: 3,
        triggers: run.triggers,
        concurrency: run.concurrency,
        singleton: run.singleton,
        onFailure: async ({ event }) => {
          // `inngest/function.failed` names the failed run.
          const failed = decodeFailedRun(event);
          if (Option.isSome(failed)) await runEffect(closeRun(failed.value.data.run_id, "failure", Option.none()));
        },
      },
      async ({ event, step, runId }) => run.handler({ event, step, runId }, runEffect),
    );

  const createCancel = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
    inngest.createFunction(
      {
        id: workflow.cancelId,
        retries: 3,
        triggers: [{ event: inngestFunctionCancelledEventType }],
        concurrency: [{ key: "event.data.run_id", limit: 1 }],
      },
      async ({ event, step }) => {
        const cancelled = await step.run("decode-cancellation", () =>
          decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event));
        if (cancelled.data.function_id !== run.id) return { skipped: true };
        const { run_id: runId, event: triggering } = cancelled.data;
        return {
          closed: await step.run("close-run", () => runEffect(closeRun(runId, "cancellation", decodeTriggering(triggering)))),
        };
      },
    );

  const createSweep = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
    inngest.createFunction(
      {
        id: sweep.id,
        retries: 3,
        triggers: [{ cron: "TZ=UTC 0 * * * *" }],
        concurrency: [{ limit: 1 }],
      },
      async ({ step }) => {
        const swept = await step.run("close-stale", () => runEffect(sweep.closeStale()));
        return sweep.afterSweep === undefined ? swept : { ...swept, ...await sweep.afterSweep(step, runEffect) };
      },
    );

  return {
    createRun,
    createCancel,
    createSweep,
    createFunctions: (inngest: PloyzInngest) => [createRun(inngest), createCancel(inngest), createSweep(inngest)] as const,
  };
}
