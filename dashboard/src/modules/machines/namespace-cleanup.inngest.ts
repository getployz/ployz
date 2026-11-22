import { Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { StepTools } from "#/modules/inngest/client";
import { namespaceCleanupRequested } from "#/modules/machines/machines.events";
import {
  bindCleanupRun,
  closeCleanupRun,
  closeStaleCleanups,
  executeCleanupOnce,
} from "#/modules/machines/namespace-cleanup.server";
import type { EffectRunner } from "#/modules/inngest/run.server";
import { eventData } from "#/modules/inngest/define-event";

/**
 * One clean: bind the run to its row → claim the row, ask the Engine once and record its answer. Only the last step
 * touches the Engine, and a retry of it never asks again (see `executeCleanupOnce`).
 */
export async function executeCleanNamespace(
  { event, step, runId }: { event: { data: unknown }; step: StepTools<"run">; runId: string },
  runEffect: EffectRunner,
) {
  const decoded = eventData(namespaceCleanupRequested)(event);
  if (Option.isNone(decoded)) return { skipped: "invalid" as const };
  const request = decoded.value;
  const bound = await step.run("bind-run", () => runEffect(bindCleanupRun(request, runId)));
  if (bound.kind === "settled") return { cleanupId: request.cleanupId, state: bound.state };
  return step.run("execute-once", () => runEffect(executeCleanupOnce(request, runId)));
}

const lifecycle = attemptLifecycle({
  run: {
    id: "clean-namespace",
    triggers: [{ event: namespaceCleanupRequested.eventType }],
    handler: executeCleanNamespace,
  },
  cancelId: "cancel-namespace-cleanup",
  triggeringEvent: Schema.Struct({ data: Schema.Struct({ cleanupId: Schema.String }) }),
  closeRun: (runId, end, triggering) =>
    closeCleanupRun(runId, end, Option.isSome(triggering) ? triggering.value.data.cleanupId : undefined),
  sweep: { id: "close-stale-namespace-cleanups", closeStale: closeStaleCleanups },
});

export const createCleanNamespace = lifecycle.createRun;
export const createCancelNamespaceCleanup = lifecycle.createCancel;
export const createNamespaceCleanupFunctions = lifecycle.createFunctions;
