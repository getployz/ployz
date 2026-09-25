import { Effect, Option, Schema } from "effect";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { serverPolicyChangeRequestedEventType } from "#/modules/inngest/events";
import { ServerPolicyChangeSchema } from "#/modules/machines/server-policy";
import { applyServerPolicyChangeActivity } from "#/modules/machines/server-policy.server";
import { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run">;
type EffectRunner = typeof runInngestEffect;

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const ServerPolicyChangeRequestedData = Schema.Struct({
  organizationId: NonEmptyString,
  machineId: NonEmptyString,
  change: ServerPolicyChangeSchema,
});

/**
 * Apply one queued Server Policy change through the Organization Runtime.
 * No row owns this work: the Servers page reads the result back from Runtime
 * observation, so an exhausted retry leaves the observed policy unchanged.
 */
export async function executeApplyServerPolicyChange(
  { event, step }: { event: { data: unknown }; step: StepTools },
  runEffect: EffectRunner,
) {
  const request = await step.run("normalize-server-policy-change", () => {
    const decoded = Schema.decodeUnknownOption(ServerPolicyChangeRequestedData)(
      event.data,
      { onExcessProperty: "preserve" },
    );
    return Option.isSome(decoded) ? decoded.value : null;
  });
  if (request === null) return { skipped: true };
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
      triggers: [{ event: serverPolicyChangeRequestedEventType }],
      // Changes to one Server apply in the order they were requested.
      concurrency: [{ key: "event.data.machineId", limit: 1 }],
    },
    async ({ event, step }) =>
      executeApplyServerPolicyChange({ event, step }, runEffect),
  );
