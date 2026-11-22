import { type Effect, Option, Schema } from "effect";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { inngestFunctionCancelledEnvelopeSchema, inngestFunctionCancelledEventType } from "#/modules/inngest/events";
import type { AppServices } from "#/server/runtime.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

type Activity<A> = Effect.Effect<A, Error, AppServices>;
type RunTrigger = Parameters<PloyzInngest["createFunction"]>[0]["triggers"];
type RunConcurrency = Parameters<PloyzInngest["createFunction"]>[0]["concurrency"];
type RunRetries = Parameters<PloyzInngest["createFunction"]>[0]["retries"];
type RunCancelOn = Parameters<PloyzInngest["createFunction"]>[0]["cancelOn"];

export type RunEnd = "failure" | "cancellation";

const decodeFailedRun = Schema.decodeUnknownOption(
  Schema.Struct({ data: Schema.Struct({ run_id: Schema.String, event: Schema.optional(Schema.Unknown) }) }),
);

/**
 * The run and cancel functions of work that owns an active-status row: a run that fails for good or is cancelled closes
 * its row through `closeRun`, so no row is left active. Work with an hourly sweep adds it through `attemptLifecycle`.
 */
export function attemptRunLifecycle<Result, Triggering = never>(workflow: {
  readonly run: {
    readonly id: string;
    /** Retries of the run itself; the cancel and sweep functions keep 3. */
    readonly retries?: RunRetries;
    readonly triggers: RunTrigger;
    readonly concurrency?: RunConcurrency;
    readonly cancelOn?: RunCancelOn;
    readonly handler: (
      ctx: { event: { data: unknown }; step: PloyzStepTools; runId: string; attempt: number },
      runEffect: EffectRunner,
    ) => Promise<Result>;
  };
  readonly cancelId: string;
  /** The cancel function's step ids, where they must stay what a run in flight already memoized. */
  readonly cancelSteps?: { readonly decode: string; readonly close: string };
  readonly triggeringEvent?: Schema.ConstraintDecoder<Triggering>;
  /** Hand a failed run's triggering event to `closeRun` too, so it can close a row the run never bound. Off, a failed run closes only rows it bound. */
  readonly triggeringOnFailure?: boolean;
  readonly closeRun: (runId: string, end: RunEnd, triggering: Option.Option<Triggering>, error?: Error) => Activity<number>;
}) {
  const { run, closeRun, triggeringEvent, triggeringOnFailure = false } = workflow;
  const cancelSteps = workflow.cancelSteps ?? { decode: "decode-cancellation", close: "close-run" };
  const decodeTriggering = triggeringEvent === undefined ? () => Option.none() : Schema.decodeUnknownOption(triggeringEvent);
  const createRun = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
    inngest.createFunction(
      {
        id: run.id,
        retries: run.retries ?? 3,
        triggers: run.triggers,
        concurrency: run.concurrency,
        cancelOn: run.cancelOn,
        onFailure: async ({ event, error }) => {
          // `inngest/function.failed` names the failed run.
          const failed = decodeFailedRun(event);
          if (Option.isSome(failed)) {
            const { run_id: runId, event: triggering } = failed.value.data;
            await runEffect(closeRun(runId, "failure", triggeringOnFailure ? decodeTriggering(triggering) : Option.none(), error));
          }
        },
      },
      async ({ event, step, runId, attempt }) => run.handler({ event, step, runId, attempt }, runEffect),
    );

  const createCancel = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) => {
    const runFunctionId = createRun(inngest).id(inngest.id);
    return inngest.createFunction(
      {
        id: workflow.cancelId,
        retries: 3,
        triggers: [{ event: inngestFunctionCancelledEventType }],
        concurrency: [{ key: "event.data.run_id", limit: 1 }],
      },
      async ({ event, step }) => {
        const cancelled = await step.run(cancelSteps.decode, () =>
          decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event));
        if (cancelled.data.function_id !== runFunctionId) return { skipped: true };
        const { run_id: runId, event: triggering } = cancelled.data;
        return {
          closed: await step.run(cancelSteps.close, () => runEffect(closeRun(runId, "cancellation", decodeTriggering(triggering)))),
        };
      },
    );
  };

  return { createRun, createCancel };
}

export function attemptLifecycle<Result, More extends object, Triggering = never>(workflow: Parameters<typeof attemptRunLifecycle<Result, Triggering>>[0] & {
  readonly sweep: {
    readonly id: string;
    readonly closeStale: () => Activity<Readonly<Record<string, number>>>;
    readonly afterSweep?: (step: PloyzStepTools, runEffect: EffectRunner) => Promise<More>;
  };
}) {
  const { createRun, createCancel } = attemptRunLifecycle(workflow);
  const { sweep } = workflow;
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
