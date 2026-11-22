import { Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { StepTools } from "#/modules/inngest/client";
import { clusterDomainSyncRequested } from "#/modules/cluster-domain/cluster-domain.events";
import { ingressDoorsRequested } from "#/modules/ingress/ingress.events";
import { bindDoorsRun, closeDoorsRun, closeStaleDoors, executeDoorsOnce } from "#/modules/ingress/doors.server";
import type { EffectRunner } from "#/modules/inngest/run.server";
import { eventData } from "#/modules/inngest/define-event";

/**
 * One switch of the Cluster's web traffic: bind the run to its row → claim the row, move the Doors and the Cluster
 * Domain and record the answer. Only the last step touches the Engine, and a retry of it never asks again (see
 * `executeDoorsOnce`). Then DNS catches up: the Cluster Domain's source changed, so the Hosted records follow it.
 */
export async function applyIngressDoors(
  { event, step, runId }: { event: { data: unknown }; step: StepTools<"run" | "sendEvent">; runId: string },
  runEffect: EffectRunner,
) {
  const decoded = eventData(ingressDoorsRequested)(event);
  if (Option.isNone(decoded)) return { skipped: "invalid" as const };
  const request = decoded.value;
  const bound = await step.run("bind-run", () => runEffect(bindDoorsRun(request, runId)));
  if (bound.kind === "settled") return { attemptId: request.attemptId, state: bound.state };
  const ended = await step.run("execute-once", () => runEffect(executeDoorsOnce(request, runId)));
  await step.sendEvent("sync-cluster-domain", clusterDomainSyncRequested.create({ organizationId: request.organizationId }));
  return ended;
}

const lifecycle = attemptLifecycle({
  run: {
    id: "apply-ingress-doors",
    triggers: [{ event: ingressDoorsRequested.eventType }],
    // A switch moves the whole Cluster, so two switches in one Organization take turns.
    concurrency: [{ key: "event.data.organizationId", limit: 1 }],
    handler: applyIngressDoors,
  },
  cancelId: "cancel-ingress-doors",
  triggeringEvent: Schema.Struct({ data: Schema.Struct({ attemptId: Schema.String }) }),
  closeRun: (runId, end, triggering) =>
    closeDoorsRun(runId, end, Option.isSome(triggering) ? triggering.value.data.attemptId : undefined),
  sweep: { id: "close-stale-ingress-doors", closeStale: closeStaleDoors },
});

export const createIngressDoorsFunctions = lifecycle.createFunctions;
