import { Option, Schema } from "effect";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
  serverDrainRequestedEventType,
} from "#/modules/inngest/events";
import { machineIdStringSchema } from "#/modules/machines/enrollment";
import {
  bindDrainRun,
  closeDrainRun,
  closeStaleDrains,
  executeDrainOnce,
  prepareDrain,
} from "#/modules/machines/server-drain.server";
import { runInngestEffect } from "#/server/run.server";

export const DRAIN_SERVER_FUNCTION_ID = "drain-server";

/** One Organization's Drain slot. `env` scope shares it across functions; `account` would share it across environments. */
export const DRAIN_SLOT = { scope: "env", key: '"server-drain-" + event.data.organizationId', limit: 1 } as const;

type StepTools = Pick<PloyzStepTools, "run">;
type EffectRunner = typeof runInngestEffect;

const ServerDrainRequestedData = Schema.Struct({
  attemptId: Schema.String.check(Schema.isUUID()),
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  machineId: machineIdStringSchema,
});

const decodeFailedRun = Schema.decodeUnknownOption(Schema.Struct({ data: Schema.Struct({ run_id: Schema.String }) }));
const decodeCancelledRequest = Schema.decodeUnknownOption(Schema.Struct({ data: Schema.Struct({ attemptId: Schema.String }) }));

/**
 * One Drain: bind the run to its row → read what the Drain selects → claim the row, ask the Engine once and record its
 * answer. Only the last step touches the Engine, and a retry of it never asks again (see `executeDrainOnce`).
 */
export async function executeDrainServer(
  { event, step, runId }: { event: { data: unknown }; step: StepTools; runId: string },
  runEffect: EffectRunner,
) {
  const request = await step.run("normalize-request", () => {
    const decoded = Schema.decodeUnknownOption(ServerDrainRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value : null;
  });
  if (request === null) return { skipped: "invalid" as const };
  const bound = await step.run("bind-run", () => runEffect(bindDrainRun(request, runId)));
  if (bound.kind === "settled") return { attemptId: request.attemptId, state: bound.state };
  const scope = await step.run("prepare", () => runEffect(prepareDrain(request.organizationId)));
  return step.run("execute-once", () => runEffect(executeDrainOnce(request, runId, scope)));
}

/** A cancelled Drain run must not leave its row active, even one it was cancelled before binding. */
export async function executeCancelServerDrain({ event, step }: { event: unknown; step: StepTools }, runEffect: EffectRunner) {
  const decoded = await step.run("decode-cancellation", () => decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event));
  if (decoded.data.function_id !== DRAIN_SERVER_FUNCTION_ID) return { skipped: true };
  const runId = decoded.data.run_id;
  const request = decodeCancelledRequest(decoded.data.event);
  const attemptId = Option.isSome(request) ? request.value.data.attemptId : undefined;
  return { closed: await step.run("close-run", () => runEffect(closeDrainRun(runId, "cancellation", attemptId))) };
}

export const createDrainServer = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: DRAIN_SERVER_FUNCTION_ID,
      retries: 3,
      triggers: [{ event: serverDrainRequestedEventType }],
      // Drains in one Organization run one at a time: each sees the Containers the one before it moved. A Server
      // Policy change shares the slot, so it never applies while a Drain runs.
      concurrency: [DRAIN_SLOT],
      onFailure: async ({ event }) => {
        // `inngest/function.failed` names the failed run.
        const failed = decodeFailedRun(event);
        if (Option.isSome(failed)) await runEffect(closeDrainRun(failed.value.data.run_id, "failure"));
      },
    },
    async ({ event, step, runId }) => executeDrainServer({ event, step, runId }, runEffect),
  );

export const createCancelServerDrain = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "cancel-server-drain",
      retries: 3,
      triggers: [{ event: inngestFunctionCancelledEventType }],
      concurrency: [{ key: "event.data.run_id", limit: 1 }],
    },
    async ({ event, step }) => executeCancelServerDrain({ event, step }, runEffect),
  );

/** The hourly sweep: a request no run picked up in time never starts; a Drain running for a day lost its run. */
export const createCloseStaleServerDrains = (inngest: PloyzInngest, runEffect: EffectRunner = runInngestEffect) =>
  inngest.createFunction(
    {
      id: "close-stale-server-drains",
      retries: 3,
      triggers: [{ cron: "TZ=UTC 0 * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => step.run("close-stale-drains", () => runEffect(closeStaleDrains())),
  );
