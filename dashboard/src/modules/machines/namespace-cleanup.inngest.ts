import { Option, Schema } from "effect";
import { attemptLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { PloyzStepTools } from "#/modules/inngest/client";
import { namespaceCleanupRequestedEventType } from "#/modules/inngest/events";
import {
  bindCleanupRun,
  closeCleanupRun,
  closeStaleCleanups,
  executeCleanupOnce,
} from "#/modules/machines/namespace-cleanup.server";
import type { runInngestEffect } from "#/server/run.server";

type StepTools = Pick<PloyzStepTools, "run">;
type EffectRunner = typeof runInngestEffect;

const NamespaceCleanupRequestedData = Schema.Struct({
  cleanupId: Schema.String.check(Schema.isUUID()),
  organizationId: Schema.String.check(Schema.isNonEmpty()),
});

/**
 * One clean: bind the run to its row → claim the row, ask the Engine once and record its answer. Only the last step
 * touches the Engine, and a retry of it never asks again (see `executeCleanupOnce`).
 */
export async function executeCleanNamespace(
  { event, step, runId }: { event: { data: unknown }; step: StepTools; runId: string },
  runEffect: EffectRunner,
) {
  const request = await step.run("normalize-request", () => {
    const decoded = Schema.decodeUnknownOption(NamespaceCleanupRequestedData)(event.data, { onExcessProperty: "preserve" });
    return Option.isSome(decoded) ? decoded.value : null;
  });
  if (request === null) return { skipped: "invalid" as const };
  const bound = await step.run("bind-run", () => runEffect(bindCleanupRun(request, runId)));
  if (bound.kind === "settled") return { cleanupId: request.cleanupId, state: bound.state };
  return step.run("execute-once", () => runEffect(executeCleanupOnce(request, runId)));
}

const lifecycle = attemptLifecycle({
  run: {
    id: "clean-namespace",
    triggers: [{ event: namespaceCleanupRequestedEventType }],
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
export const createCloseStaleNamespaceCleanups = lifecycle.createSweep;
export const createNamespaceCleanupFunctions = lifecycle.createFunctions;
