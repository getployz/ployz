import { Effect, Option, Schema } from "effect";
import { clusterDomainSyncRequested } from "#/modules/cluster-domain/cluster-domain.events";
import { machineRemoveRequested } from "#/modules/machines/machines.events";
import { attemptRunLifecycle } from "#/modules/inngest/attempt-lifecycle";
import type { PloyzInngest, StepTools } from "#/modules/inngest/client";
import { eventData } from "#/modules/inngest/define-event";
import {
  cancelMachineRemoveAttemptActivity,
  failOwnedMachineRemoveAttemptActivity,
  observeLeavingServerActivity,
  prepareMachineRemoveAttemptActivity,
  releaseLeavingLanesActivity,
  releaseServerActivity,
  removeMachineActivity,
} from "#/modules/machines/machine-removal.server";
import { completeMachineRemoveAttempt } from "#/modules/machines/machine-removal.repository";
import { observeOrganization } from "#/modules/volume-run/volume-run.orphans.server";
import { type EffectRunner, runInngestEffect } from "#/modules/inngest/run.server";

export const MACHINE_REMOVE_RETRIES = 5;

export async function executeProcessMachineRemove({
  event,
  step,
  runId,
  attempt: stepAttempt,
}: {
  event: { data: unknown };
  step: StepTools<"run" | "sendEvent">;
  runId: string;
  attempt: number;
}, runEffect: EffectRunner, observe: typeof observeOrganization) {
  const requested = eventData(machineRemoveRequested)(event);
  if (Option.isNone(requested)) {
    return { attemptId: null, status: "invalid", skipped: true };
  }
  const { attemptId } = requested.value;

  const prepared = await step.run("claim-machine-remove", () =>
    runEffect(
      prepareMachineRemoveAttemptActivity({
        attemptId,
        inngestRunId: runId,
        now: new Date(),
      }),
    ),
  );
  if (prepared.kind === "missing") {
    return { attemptId, status: "missing", skipped: true };
  }
  if (prepared.kind === "terminal") {
    return {
      attemptId: prepared.attempt.id,
      status: prepared.attempt.state,
      skipped: true,
    };
  }

  const attempt = prepared.attempt;
  // Read before the removal: once the Server is gone the Cluster no longer names it or lists its copies.
  const leaving = await step.run("observe-leaving-server", () =>
    runEffect(observeLeavingServerActivity({
      organizationId: attempt.organizationId,
      machineId: attempt.machineId,
      lastTry: stepAttempt >= MACHINE_REMOVE_RETRIES,
    }, observe)),
  );
  const removed = await step.run("remove-machine", () =>
    runEffect(Effect.scoped(removeMachineActivity({ ...attempt, inngestRunId: runId }))),
  );

  // The attempt is no longer this run's running attempt: it finished, was cancelled or failed, or another run claimed
  // it. This run asked nothing and leaves the outcome alone.
  if (removed.kind === "superseded") {
    return { attemptId, status: "superseded", skipped: true };
  }

  if (removed.kind === "refused") {
    await step.run("complete-refused", () =>
      runEffect(
        completeMachineRemoveAttempt({
          attemptId,
          inngestRunId: runId,
          completion: { state: "failed", failureCode: removed.failureCode, failureMessage: removed.message },
          now: new Date(),
        }),
      ),
    );
    return { attemptId, status: "failed" as const };
  }

  if (removed.kind === "missing_identities") {
    await step.run("complete-missing-identities", () =>
      runEffect(
        completeMachineRemoveAttempt({
          attemptId,
          inngestRunId: runId,
          completion: {
            state: "missing_identities",
            identities: removed.identities,
          },
          now: new Date(),
        }),
      ),
    );
    return { attemptId, status: "missing_identities" as const };
  }
  await step.run("release-volume-lanes", () =>
    runEffect(releaseLeavingLanesActivity({
      organizationId: attempt.organizationId,
      machineId: attempt.machineId,
      name: leaving.machine.name,
      writes: leaving.writes,
    })),
  );
  // Its own step: a retry after the Forget releases again, never forgets a Server twice.
  const release = await step.run("release-server", () =>
    runEffect(
      releaseServerActivity({
        organizationId: attempt.organizationId,
        machineId: attempt.machineId,
        generation: removed.generation,
      }),
    ),
  );

  await step.run("complete-succeeded", () =>
    runEffect(
      completeMachineRemoveAttempt({
        attemptId,
        inngestRunId: runId,
        completion: {
          state: "succeeded",
          result: { release },
        },
        now: new Date(),
      }),
    ),
  );
  // The removed Server may have been in the Cluster Domain's apex set.
  await step.sendEvent(
    "request-cluster-domain-sync",
    clusterDomainSyncRequested.create({ organizationId: attempt.organizationId }),
  );
  return { attemptId, status: "succeeded" as const };
}

const machineRemoveLifecycle = (observe: typeof observeOrganization) => attemptRunLifecycle({
  run: {
    id: "process-machine-remove",
    retries: MACHINE_REMOVE_RETRIES,
    triggers: [{ event: machineRemoveRequested.eventType }],
    concurrency: [{ key: "event.data.attemptId", limit: 1 }],
    handler: (ctx, runEffect) => executeProcessMachineRemove(ctx, runEffect, observe),
  },
  cancelId: "cancel-machine-remove",
  // Runs in flight have memoized these steps, so they keep their ids.
  cancelSteps: { decode: "decode-machine-remove-cancellation-event", close: "cancel-machine-remove" },
  triggeringEvent: Schema.Struct({ data: machineRemoveRequested.schema }),
  triggeringOnFailure: true,
  closeRun: (runId, end, triggering) => {
    const now = new Date();
    if (end === "cancellation") return Effect.map(cancelMachineRemoveAttemptActivity({ inngestRunId: runId, now }), closedCount);
    return Option.isNone(triggering)
      ? Effect.succeed(0)
      : Effect.map(failOwnedMachineRemoveAttemptActivity({ attemptId: triggering.value.data.attemptId, inngestRunId: runId, now }), closedCount);
  },
});

const closedCount = ({ state }: { readonly state: "skipped" | "failed" | "cancelled" }) => (state === "skipped" ? 0 : 1);

export const createProcessMachineRemove = (
  inngest: PloyzInngest,
  runEffect: EffectRunner = runInngestEffect,
  observe: typeof observeOrganization = observeOrganization,
) => machineRemoveLifecycle(observe).createRun(inngest, runEffect);

export const createCancelMachineRemove = (inngest: PloyzInngest) => machineRemoveLifecycle(observeOrganization).createCancel(inngest);
