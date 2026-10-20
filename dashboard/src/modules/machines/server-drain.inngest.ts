import { Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { DRAIN_SLOT } from "#/modules/inngest/drain-slot";
import { serverDrainRequestedEventType } from "#/modules/inngest/events";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import {
  bindDrainRun,
  closeDrainRun,
  closeStaleDrains,
  executeDrainOnce,
} from "#/modules/machines/server-drain.server";
import type { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run" | "sleep">;
type EffectRunner = typeof runInngestEffect;

/**
 * How long a Drain whose Server doesn't answer waits before it tries again: a Server that answers again is drained
 * within it, and a try spends 2 of a run's 1,000 Inngest steps, so waiting out `DRAIN_PENDING_LIMIT_MS` takes at
 * most about 90.
 */
const SERVER_WAIT = "20s";

const ServerDrainRequestedData = Schema.Struct({
  attemptId: Schema.String.check(Schema.isUUID()),
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  machineId: rustMachineIdSchema,
});

/**
 * One Drain: bind the run to its row → claim the row, ask the Engine once and record its answer. Only the last step
 * touches the Engine, and a retry of it never asks again (see `executeDrainOnce`). While the Cluster doesn't answer,
 * that step leaves the row pending, and the run waits and tries it again, until the request's pending limit ends it.
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
  for (let tried = 0; ; tried += 1) {
    if (tried > 0) await step.sleep(`wait-for-server-${tried}`, SERVER_WAIT);
    const reply = await step.run(
      tried === 0 ? "execute-once" : `execute-once-${tried}`,
      () => runEffect(executeDrainOnce(request, runId, { waited: tried > 0 })),
    );
    if (reply.state !== "pending") return reply;
  }
}

const lifecycle = attemptLifecycle({
  run: {
    id: "drain-server",
    triggers: [{ event: serverDrainRequestedEventType }],
    concurrency: [DRAIN_SLOT],
    handler: executeDrainServer,
  },
  cancelId: "cancel-server-drain",
  triggeringEvent: Schema.Struct({ data: Schema.Struct({ attemptId: Schema.String }) }),
  closeRun: (runId, end, triggering) =>
    closeDrainRun(runId, end, Option.isSome(triggering) ? triggering.value.data.attemptId : undefined),
  sweep: { id: "close-stale-server-drains", closeStale: closeStaleDrains },
});

export const createDrainServer = lifecycle.createRun;
export const createCancelServerDrain = lifecycle.createCancel;
export const createCloseStaleServerDrains = lifecycle.createSweep;
export const createServerDrainFunctions = lifecycle.createFunctions;
