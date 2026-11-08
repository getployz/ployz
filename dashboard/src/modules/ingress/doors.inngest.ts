import { Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { createClusterDomainSyncRequestedEvent, ingressDoorsRequestedEventType } from "#/modules/inngest/events";
import { bindDoorsRun, closeDoorsRun, closeStaleDoors, executeDoorsOnce } from "#/modules/ingress/doors.server";
import type { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run" | "sendEvent">;
type EffectRunner = typeof runInngestEffect;

const IngressDoorsRequestedData = Schema.Struct({
  attemptId: Schema.String.check(Schema.isUUID()),
  organizationId: Schema.String.check(Schema.isNonEmpty()),
});

/**
 * One switch of the Cluster's web traffic: bind the run to its row → claim the row, move the Doors and the Cluster
 * Domain and record the answer. Only the last step touches the Engine, and a retry of it never asks again (see
 * `executeDoorsOnce`). Then DNS catches up: the Cluster Domain's source changed, so the Hosted records follow it.
 */
export async function applyIngressDoors(
  { event, step, runId }: { event: { data: unknown }; step: StepTools; runId: string },
  runEffect: EffectRunner,
) {
  const request = await step.run("normalize-request", () => {
    const decoded = Schema.decodeUnknownOption(IngressDoorsRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value : null;
  });
  if (request === null) return { skipped: "invalid" as const };
  const bound = await step.run("bind-run", () => runEffect(bindDoorsRun(request, runId)));
  if (bound.kind === "settled") return { attemptId: request.attemptId, state: bound.state };
  const ended = await step.run("execute-once", () => runEffect(executeDoorsOnce(request, runId)));
  await step.sendEvent("sync-cluster-domain", createClusterDomainSyncRequestedEvent({ organizationId: request.organizationId }));
  return ended;
}

const lifecycle = attemptLifecycle({
  run: {
    id: "apply-ingress-doors",
    triggers: [{ event: ingressDoorsRequestedEventType }],
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

export const createApplyIngressDoors = lifecycle.createRun;
export const createCancelIngressDoors = lifecycle.createCancel;
export const createCloseStaleIngressDoors = lifecycle.createSweep;
export const createIngressDoorsFunctions = lifecycle.createFunctions;
