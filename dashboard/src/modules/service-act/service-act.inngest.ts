import { Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { serviceActRequestedEventType } from "#/modules/inngest/events";
import { cameBack, serversOf, stepsOf } from "#/modules/service-act/service-act";
import {
  actOnServer,
  claimServiceAct,
  closeServiceActRun,
  closeStaleServiceActs,
  recordServiceAct,
} from "#/modules/service-act/service-act.server";
import type { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run">;
type EffectRunner = typeof runInngestEffect;

const ServiceActRequestedData = Schema.Struct({
  actId: Schema.String.check(Schema.isUUID()),
  organizationId: Schema.String.check(Schema.isNonEmpty()),
});

export async function executeServiceAct(
  { event, step, runId }: { event: { data: unknown }; step: StepTools; runId: string },
  runEffect: EffectRunner,
) {
  const request = await step.run("normalize-request", () => {
    const decoded = Schema.decodeUnknownOption(ServiceActRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value : null;
  });
  if (request === null) return { skipped: "invalid" as const };
  const claimed = await step.run("claim", () => runEffect(claimServiceAct(request, runId)));
  if (claimed.kind === "settled") return { actId: request.actId, state: claimed.state };
  const { stop, start } = stepsOf(claimed.verb);
  for (const server of serversOf(claimed.legs)) {
    if (stop) await step.run(`stop:${server}`, () => runEffect(actOnServer(request, runId, server, "stop")));
    if (!start) continue;
    const legs = await step.run(`start:${server}`, () => runEffect(actOnServer(request, runId, server, "start")));
    if (stop && !cameBack(legs, server)) break;
  }
  return step.run("record", () => runEffect(recordServiceAct(request, runId)));
}

const lifecycle = attemptLifecycle({
  run: {
    id: "act-service",
    triggers: [{ event: serviceActRequestedEventType }],
    handler: executeServiceAct,
  },
  cancelId: "cancel-service-act",
  triggeringEvent: Schema.Struct({ data: Schema.Struct({ actId: Schema.String }) }),
  closeRun: (runId, end, triggering) =>
    closeServiceActRun(runId, end, Option.isSome(triggering) ? triggering.value.data.actId : undefined),
  sweep: { id: "close-stale-service-acts", closeStale: closeStaleServiceActs },
});

export const createActService = lifecycle.createRun;
export const createServiceActFunctions = lifecycle.createFunctions;
