import { Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { StepTools } from "#/modules/inngest/client";
import { DRAIN_SLOT } from "#/modules/machines/drain-slot";
import { type ServerDrainRequestedEventData, serverDrainRequested } from "#/modules/machines/machines.events";
import { admitDrainVolume, type DrainVolume, drainMoveState, finishDrain, planDrainVolumes } from "#/modules/machines/server-drain-volumes.server";
import { inngestFunctionFinishedEventType } from "#/modules/inngest/events";
import { finishedVolumeRun } from "#/modules/volume-run/volume-run.inngest";
import { closeVolumeRun } from "#/modules/volume-run/volume-run.rows.server";
import {
  bindDrainRun,
  closeDrainRun,
  closeStaleDrains,
  executeDrainOnce,
} from "#/modules/machines/server-drain.server";
import type { EffectRunner } from "#/modules/inngest/run.server";
import { eventData } from "#/modules/inngest/define-event";

/**
 * How long a Drain whose Server doesn't answer waits before it tries again: a Server that answers again is drained
 * within it, and a try spends 2 of a run's 1,000 Inngest steps, so waiting out `DRAIN_PENDING_LIMIT_MS` takes at
 * most about 90.
 */
const SERVER_WAIT = "20s";

/**
 * One Drain: bind the run to its row → claim the row, ask the Engine once and keep its answer on the running row → plan
 * the Volumes it left on the Server → move each, one at a time, through a Volume Run → end the row with the final
 * report. Only `execute-once` touches the Engine, and a retry of it never asks again (see `executeDrainOnce`). While
 * the Cluster doesn't answer, that step leaves the row pending, and the run waits and tries it again, until the
 * request's pending limit ends it.
 */
export async function executeDrainServer(
  { event, step, runId }: { event: { data: unknown }; step: Steps; runId: string },
  runEffect: EffectRunner,
) {
  const decoded = eventData(serverDrainRequested)(event);
  if (Option.isNone(decoded)) return { skipped: "invalid" as const };
  const request = decoded.value;
  const bound = await step.run("bind-run", () => runEffect(bindDrainRun(request, runId)));
  if (bound.kind === "settled") return { attemptId: request.attemptId, state: bound.state };
  for (let tried = 0; ; tried += 1) {
    if (tried > 0) await step.sleep(`wait-for-server-${tried}`, SERVER_WAIT);
    const reply = await step.run(
      tried === 0 ? "execute-once" : `execute-once-${tried}`,
      () => runEffect(executeDrainOnce(request, runId, { waited: tried > 0 })),
    );
    if (reply.state === "pending") continue;
    if (reply.state !== "running") return reply;
    break;
  }
  const volumes = await step.run("volumes-plan", () => runEffect(planDrainVolumes(request, runId)));
  for (const item of volumes) {
    const { admitted } = await step.run(`volume-${item.volumeId}`, () => runEffect(admitDrainVolume(request, runId, item)));
    if (admitted) await waitForMove({ step, runId, request, item }, runEffect);
  }
  return step.run("finish", () => runEffect(finishDrain(request, runId)));
}

type Steps = StepTools<"run" | "sleep" | "waitForEvent">;

/** A Deploy waits on a Volume Run this long at a time, and so does a drain. */
const MOVE_WAIT_MS = 10 * 60_000;

/**
 * Waits on the drain's move of `item`, then on the Release its end queued, until both ended or the drain's time is up.
 * The finished event names how a run ended, so the drain closes that run's row itself rather than wait for the closer;
 * a wait that times out looks at the rows again, since the event can be lost.
 */
async function waitForMove(
  { step, runId, request, item }: { step: Steps; runId: string; request: ServerDrainRequestedEventData; item: DrainVolume },
  runEffect: EffectRunner,
) {
  for (let check = 0; ; check += 1) {
    const seen = await step.run(`volume-${item.volumeId}-check-${check}`, () => runEffect(drainMoveState(request, runId, item)));
    if (seen.waitFor === null || seen.leftMs <= 0) return;
    const finished = await step.waitForEvent(`volume-${item.volumeId}-wait-${check}`, {
      event: inngestFunctionFinishedEventType,
      if: `async.data.event.data.runId == '${seen.waitFor}'`,
      timeout: `${Math.ceil(Math.min(MOVE_WAIT_MS, seen.leftMs) / 1000)}s`,
    });
    if (finished !== null) {
      const { runId: ended, end } = finishedVolumeRun(finished);
      await step.run(`volume-${item.volumeId}-close-${check}`, () => runEffect(closeVolumeRun(ended, end)));
    }
  }
}

const lifecycle = attemptLifecycle({
  run: {
    id: "drain-server",
    triggers: [{ event: serverDrainRequested.eventType }],
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
