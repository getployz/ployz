import { Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { createClusterDomainSyncRequestedEvent, serverWebTrafficRequestedEventType } from "#/modules/inngest/events";
import { rustMachineIdSchema } from "#/modules/machines/enrollment";
import {
  bindWebTrafficRun,
  closeStaleWebTraffic,
  closeWebTrafficRun,
  executeWebTrafficOnce,
} from "#/modules/machines/server-web-traffic.server";
import type { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run" | "sendEvent">;
type EffectRunner = typeof runInngestEffect;

const ServerWebTrafficRequestedData = Schema.Struct({
  attemptId: Schema.String.check(Schema.isUUID()),
  organizationId: Schema.String.check(Schema.isNonEmpty()),
  machineId: rustMachineIdSchema,
});

/**
 * One web traffic change: bind the run to its row → claim the row, ask the Engine once and record its answer. Only
 * the last step touches the Engine, and a retry of it never asks again (see `executeWebTrafficOnce`). Then DNS catches
 * up: turning the role back on over a proxy that kept running changes no container, so nothing else would tell it.
 */
export async function applyServerWebTraffic(
  { event, step, runId }: { event: { data: unknown }; step: StepTools; runId: string },
  runEffect: EffectRunner,
) {
  const request = await step.run("normalize-request", () => {
    const decoded = Schema.decodeUnknownOption(ServerWebTrafficRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value : null;
  });
  if (request === null) return { skipped: "invalid" as const };
  const bound = await step.run("bind-run", () => runEffect(bindWebTrafficRun(request, runId)));
  if (bound.kind === "settled") return { attemptId: request.attemptId, state: bound.state };
  const ended = await step.run("execute-once", () => runEffect(executeWebTrafficOnce(request, runId)));
  await step.sendEvent("sync-cluster-domain", createClusterDomainSyncRequestedEvent({ organizationId: request.organizationId }));
  return ended;
}

const lifecycle = attemptLifecycle({
  run: {
    id: "apply-server-web-traffic",
    triggers: [{ event: serverWebTrafficRequestedEventType }],
    // The start waits for the whole Cluster to see the role, so two Servers' changes in one Organization take turns.
    concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    handler: applyServerWebTraffic,
  },
  cancelId: "cancel-server-web-traffic",
  triggeringEvent: Schema.Struct({ data: Schema.Struct({ attemptId: Schema.String }) }),
  closeRun: (runId, end, triggering) =>
    closeWebTrafficRun(runId, end, Option.isSome(triggering) ? triggering.value.data.attemptId : undefined),
  sweep: { id: "close-stale-server-web-traffic", closeStale: closeStaleWebTraffic },
});

export const createApplyServerWebTraffic = lifecycle.createRun;
export const createCancelServerWebTraffic = lifecycle.createCancel;
export const createCloseStaleServerWebTraffic = lifecycle.createSweep;
export const createServerWebTrafficFunctions = lifecycle.createFunctions;
