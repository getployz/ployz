import { Effect, Option, Schema } from "effect";
import {
  inngestEventEnvelopeFields,
  inngestFunctionCancelledEnvelopeSchema,
  inngestFunctionCancelledEventType,
  inngestFunctionFailedEnvelopeSchema,
  teardownRequestedEventType,
} from "#/modules/inngest/events";
import type {
  PloyzInngest,
  PloyzStepTools,
} from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import { PROCESS_TEARDOWN_FUNCTION_ID } from "#/modules/inngest/row-backed-workflow-ids";
import {
  incompleteTeardownOutcome,
  teardownOutcome,
  type TeardownOutcome,
} from "#/modules/runtime/teardown";
import {
  cancelTeardownAttemptActivity,
  completeTeardownAttemptActivity,
  destroyClusterActivity,
  destroyEnvironmentActivity,
  dropTeardownCloudRowsActivity,
  failOwnedTeardownAttemptActivity,
  prepareTeardownAttemptActivity,
  recordTeardownRuntimeEvidenceActivity,
  revokeTeardownPairingActivity,
} from "#/modules/runtime/teardown-activities.server";
import { runInngestEffect } from "#/server/run.server";
import { reviveDurableAttemptDates } from "#/modules/runtime/durable-attempt-dates";

export { PROCESS_TEARDOWN_FUNCTION_ID };

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const TeardownRequestedData = Schema.Struct({ attemptId: NonEmptyString });
const TeardownRequestedEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal("cloud/teardown.requested"),
  data: TeardownRequestedData,
});
const TeardownFailureEnvelope = inngestFunctionFailedEnvelopeSchema(
  TeardownRequestedEnvelope,
);

export const decodeTeardownFailureEvent = Schema.decodeUnknownOption(
  TeardownFailureEnvelope,
);

type StepTools = Pick<PloyzStepTools, "run">;

export async function executeProcessTeardown({
  event,
  step,
  runId,
}: {
  event: { data: unknown };
  step: StepTools;
  runId: string;
}) {
  const attemptId = await step.run("normalize-teardown-attempt-id", () => {
    const decoded = Schema.decodeUnknownOption(TeardownRequestedData)(
      event.data,
      { onExcessProperty: "preserve" },
    );
    return Option.isSome(decoded) ? decoded.value.attemptId : null;
  });
  if (attemptId === null) {
    return { attemptId: null, status: "invalid", skipped: true };
  }

  const serialized = await step.run("claim-teardown", () =>
    runInngestEffect(
      prepareTeardownAttemptActivity({
        attemptId,
        inngestRunId: runId,
        now: new Date(),
      }),
    ),
  );
  const prepared = serialized.kind === "missing"
    ? serialized
    : {
        ...serialized,
        attempt: reviveDurableAttemptDates(serialized.attempt),
      };
  if (prepared.kind === "missing") {
    return { attemptId, status: "missing", skipped: true };
  }
  if (prepared.kind === "terminal") {
    return {
      attemptId: prepared.attempt.id,
      status: prepared.attempt.status,
      skipped: true,
    };
  }

  const attempt = prepared.attempt;
  const membership = attempt.targets.runtimeMembership;
  let outcome: TeardownOutcome;
  let finalRuntimeEvidenceRecorded = false;
  if (attempt.scope === "organization" && membership === "verified") {
    const destroyed = await step.run("destroy-cluster", () =>
      runInngestEffect(
        Effect.scoped(
          destroyClusterActivity({
            organizationId: attempt.organizationId,
            confirmDataLoss: attempt.confirmDataLoss,
          }),
        ),
      ),
    );
    const revocation = await step.run("revoke-pairing", () => runInngestEffect(
      revokeTeardownPairingActivity({ organizationId: attempt.organizationId }),
    ));
    const clusterTeardown = { ...destroyed, pairing_revoked: !revocation.pairingRevocationUnconfirmed };
    const machineTeardownIncomplete =
      clusterTeardown.machines.failures.length > 0 ||
      clusterTeardown.machines.omissions.length > 0;
    const clusterOutcome =
      machineTeardownIncomplete || !clusterTeardown.pairing_revoked
        ? clusterTeardown.pairing_revoked
          ? incompleteTeardownOutcome("verified", { clusterTeardown, pairingRemovals: revocation.pairingRemovals })
          : teardownOutcome("unknown", true, { clusterTeardown, pairingRemovals: revocation.pairingRemovals })
        : teardownOutcome("verified", false, { clusterTeardown, pairingRemovals: revocation.pairingRemovals });
    await step.run("record-cluster-teardown", () =>
      runInngestEffect(
        recordTeardownRuntimeEvidenceActivity({
          attemptId: attempt.id,
          inngestRunId: runId,
          outcome: clusterOutcome,
          now: new Date(),
        }),
      ),
    );
    finalRuntimeEvidenceRecorded = true;
    if (machineTeardownIncomplete || !clusterTeardown.pairing_revoked) {
      const partial = await step.run("persist-partial-teardown-outcome", () =>
        runInngestEffect(
          completeTeardownAttemptActivity({
            attemptId: attempt.id,
            inngestRunId: runId,
            status: "partial",
            outcome: clusterOutcome,
            now: new Date(),
          }),
        ),
      );
      return { attemptId: partial.id, status: partial.status };
    }
    outcome = clusterOutcome;
  } else {
    const projectTeardowns: NonNullable<
      TeardownOutcome["projectTeardowns"]
    > = [];
    if (attempt.targets.destroyRuntimeProjects) {
      for (const target of attempt.targets.environments) {
        const projectTeardown = await step.run(
          `destroy-environment-${target.environmentId}`,
          () =>
            runInngestEffect(
              Effect.scoped(
                destroyEnvironmentActivity({
                  organizationId: attempt.organizationId,
                  target,
                  confirmDataLoss: attempt.confirmDataLoss,
                }),
              ),
            ),
        );
        projectTeardowns.push({
          projectName: target.projectName,
          outcome: projectTeardown,
        });
        const projectOutcome = teardownOutcome(membership, false, {
          projectTeardowns: [...projectTeardowns],
        });
        await step.run(
          `record-project-teardown-${target.environmentId}`,
          () =>
            runInngestEffect(
              recordTeardownRuntimeEvidenceActivity({
                attemptId: attempt.id,
                inngestRunId: runId,
                outcome: projectOutcome,
                now: new Date(),
              }),
            ),
        );
        if (projectTeardown.type === "failed") {
          const partial = await step.run(
            "persist-partial-teardown-outcome",
            () =>
              runInngestEffect(
                completeTeardownAttemptActivity({
                  attemptId: attempt.id,
                  inngestRunId: runId,
                  status: "partial",
                  outcome: incompleteTeardownOutcome(membership, {
                    projectTeardowns: [...projectTeardowns],
                  }),
                  now: new Date(),
                }),
              ),
          );
          return { attemptId: partial.id, status: partial.status };
        }
      }
    }

    let pairingRevocationUnconfirmed = false;
    let pairingRemovals: TeardownOutcome["pairingRemovals"];
    if (attempt.targets.revokePairing) {
      const revoked = await step.run("revoke-pairing", () =>
        runInngestEffect(
          revokeTeardownPairingActivity({
            organizationId: attempt.organizationId,
          }),
        ),
      );
      pairingRevocationUnconfirmed = revoked.pairingRevocationUnconfirmed;
      pairingRemovals = revoked.pairingRemovals;
    }
    outcome = teardownOutcome(membership, pairingRevocationUnconfirmed, {
      projectTeardowns: [...projectTeardowns],
      pairingRemovals,
    });
  }

  if (!finalRuntimeEvidenceRecorded) {
    await step.run("record-runtime-evidence", () =>
      runInngestEffect(
        recordTeardownRuntimeEvidenceActivity({
          attemptId: attempt.id,
          inngestRunId: runId,
          outcome,
          now: new Date(),
        }),
      ),
    );
  }
  if (outcome.pairingRevocationUnconfirmed) {
    const partial = await step.run("persist-partial-teardown-outcome", () => runInngestEffect(
      completeTeardownAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: runId,
        status: "partial",
        outcome,
        now: new Date(),
      }),
    ));
    return { attemptId: partial.id, status: partial.status };
  }
  await step.run("drop-cloud-rows", () =>
    runInngestEffect(dropTeardownCloudRowsActivity(attempt)),
  );
  const completed = await step.run("persist-teardown-outcome", () =>
    runInngestEffect(
      completeTeardownAttemptActivity({
        attemptId: attempt.id,
        inngestRunId: runId,
        status: "completed",
        outcome,
        now: new Date(),
      }),
    ),
  );
  return { attemptId: completed.id, status: completed.status };
}

export async function executeProcessTeardownOnFailure({
  event,
}: {
  event: unknown;
}) {
  const decoded = decodeTeardownFailureEvent(event);
  if (Option.isNone(decoded)) return;
  await runInngestEffect(
    failOwnedTeardownAttemptActivity({
      attemptId: decoded.value.data.event.data.attemptId,
      inngestRunId: decoded.value.data.run_id,
      failureMessage:
        decoded.value.data.error.message || "Teardown retries were exhausted.",
      now: new Date(),
    }),
  );
}

export async function executeCancelTeardown({
  event,
  step,
}: {
  event: unknown;
  step: StepTools;
}) {
  const decoded = await step.run("decode-teardown-cancellation-event", () =>
    decodeInngestEnvelope(inngestFunctionCancelledEnvelopeSchema)(event),
  );
  if (decoded.data.function_id !== PROCESS_TEARDOWN_FUNCTION_ID) {
    return { skipped: true };
  }
  return step.run("cancel-teardown", () =>
    runInngestEffect(
      cancelTeardownAttemptActivity({
        inngestRunId: decoded.data.run_id,
        now: new Date(),
      }),
    ),
  );
}

export const createProcessTeardown = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: PROCESS_TEARDOWN_FUNCTION_ID,
    retries: 0,
    triggers: [{ event: teardownRequestedEventType }],
    concurrency: [{ key: "event.data.attemptId", limit: 1 }],
    onFailure: async ({ event }) =>
      executeProcessTeardownOnFailure({ event }),
  },
  async ({ event, step, runId }) =>
    executeProcessTeardown({
      event,
      step,
      runId,
    }),
  );

export const createCancelTeardown = (inngest: PloyzInngest) =>
  inngest.createFunction(
  {
    id: "cancel-teardown",
    retries: 3,
    triggers: [{ event: inngestFunctionCancelledEventType }],
    concurrency: [{ key: "event.data.run_id", limit: 1 }],
  },
  async ({ event, step }) =>
    executeCancelTeardown({ event, step }),
  );
