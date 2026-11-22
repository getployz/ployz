import { Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { StepTools } from "#/modules/inngest/client";
import { clusterDomainSyncRequested } from "#/modules/cluster-domain/cluster-domain.events";
import { serverWebTrafficRequested } from "#/modules/machines/machines.events";
import {
  bindWebTrafficRun,
  closeStaleWebTraffic,
  closeWebTrafficRun,
  executeWebTrafficOnce,
} from "#/modules/machines/server-web-traffic.server";
import type { EffectRunner } from "#/modules/inngest/run.server";
import { eventData } from "#/modules/inngest/define-event";

/**
 * One web traffic change: bind the run to its row → claim the row, ask the Engine once and record its answer. Only
 * the last step touches the Engine, and a retry of it never asks again (see `executeWebTrafficOnce`). Then DNS catches
 * up: turning the role back on over a proxy that kept running changes no container, so nothing else would tell it.
 */
export async function applyServerWebTraffic(
  { event, step, runId }: { event: { data: unknown }; step: StepTools<"run" | "sendEvent">; runId: string },
  runEffect: EffectRunner,
) {
  const decoded = eventData(serverWebTrafficRequested)(event);
  if (Option.isNone(decoded)) return { skipped: "invalid" as const };
  const request = decoded.value;
  const bound = await step.run("bind-run", () => runEffect(bindWebTrafficRun(request, runId)));
  if (bound.kind === "settled") return { attemptId: request.attemptId, state: bound.state };
  const ended = await step.run("execute-once", () => runEffect(executeWebTrafficOnce(request, runId)));
  await step.sendEvent("sync-cluster-domain", clusterDomainSyncRequested.create({ organizationId: request.organizationId }));
  return ended;
}

const lifecycle = attemptLifecycle({
  run: {
    id: "apply-server-web-traffic",
    triggers: [{ event: serverWebTrafficRequested.eventType }],
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

export const createServerWebTrafficFunctions = lifecycle.createFunctions;
