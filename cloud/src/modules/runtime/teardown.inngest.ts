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
import { identitiesForMachine, teardownOutcome } from "#/modules/runtime/teardown";
import {
  cancelTeardownAttemptActivity,
  completeTeardownAttemptActivity,
  destroyEnvironmentActivity,
  dropTeardownCloudRowsActivity,
  failOwnedTeardownAttemptActivity,
  prepareTeardownAttemptActivity,
  removeTeardownMachineActivity,
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
  for (const target of attempt.targets.environments) {
    await step.run(`destroy-environment-${target.environmentId}`, () =>
      runInngestEffect(
        Effect.scoped(
          destroyEnvironmentActivity({
            organizationId: attempt.organizationId,
            target,
          }),
        ),
      ),
    );
  }

  if (membership !== "unknown") {
    for (const machineId of attempt.targets.machines) {
      await step.run(`remove-machine-${machineId}`, () =>
        runInngestEffect(
          Effect.scoped(
            removeTeardownMachineActivity({
              organizationId: attempt.organizationId,
              machineId,
              identities: identitiesForMachine(
                attempt.confirmDataLoss,
                machineId,
              ),
            }),
          ),
        ),
      );
    }
  }

  let rustMustRevokePairing = false;
  if (attempt.targets.revokePairing) {
    const revoked = await step.run("revoke-pairing", () =>
      runInngestEffect(
        revokeTeardownPairingActivity({
          organizationId: attempt.organizationId,
          requireComplete: membership === "verified",
        }),
      ),
    );
    rustMustRevokePairing = revoked.rustMustRevokePairing;
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
        outcome: teardownOutcome(membership, rustMustRevokePairing),
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
    retries: 5,
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
