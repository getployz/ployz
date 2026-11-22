import { Effect, Option } from "effect";
import type { PloyzInngest, StepTools } from "#/modules/inngest/client";
import { eventData } from "#/modules/inngest/define-event";
import { DRAIN_SLOT } from "#/modules/machines/drain-slot";
import { serverPolicyChangeRequested } from "#/modules/machines/machines.events";
import { applyServerPolicyChangeActivity } from "#/modules/machines/server-policy.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

/**
 * Apply one queued Server Policy change through the Organization Runtime.
 * No row owns this work: the Servers page reads the result back from Runtime
 * observation, so an exhausted retry leaves the observed policy unchanged.
 */
export async function executeApplyServerPolicyChange(
  { event, step }: { event: { data: unknown }; step: StepTools<"run"> },
  runEffect: EffectRunner,
) {
  const decoded = eventData(serverPolicyChangeRequested)(event);
  if (Option.isNone(decoded)) return { skipped: true };
  const request = decoded.value;
  await step.run("update-machine", () =>
    runEffect(Effect.scoped(applyServerPolicyChangeActivity(request))),
  );
  return { machineId: request.machineId, applied: true };
}

export const createApplyServerPolicyChange = (
  inngest: PloyzInngest,
  runEffect: EffectRunner = runInngestEffect,
) =>
  inngest.createFunction(
    {
      id: "apply-server-policy-change",
      retries: 3,
      triggers: [{ event: serverPolicyChangeRequested.eventType }],
      // A change that turns services on waits out its Organization's Drain; any other takes only its Server's slot.
      // Changes to one Server apply in the order they were requested.
      concurrency: [DRAIN_SLOT, { key: "event.data.machineId", limit: 1 }],
    },
    async ({ event, step }) =>
      executeApplyServerPolicyChange({ event, step }, runEffect),
  );
