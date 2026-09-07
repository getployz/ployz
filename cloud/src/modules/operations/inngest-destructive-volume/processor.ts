import { NonRetriableError } from "inngest";
import { Option, Schema } from "effect";
import type { PloyzInngest, PloyzStepTools } from "#/modules/inngest/client";
import { decodeInngestEnvelope } from "#/modules/inngest/envelope";
import {
  inngestEventEnvelopeFields,
  inngestFunctionFailedEnvelopeSchema,
} from "#/modules/inngest/events";
import {
  isDestructiveVolumeTerminalEvidenceEvent,
  projectDestructiveVolumeTerminalAttemptEvent,
  projectDestructiveVolumeWorkflowAttempt,
} from "#/modules/operations/destructive-volume-attempt";
import {
  attachDestructiveVolumeOperation,
  claimDestructiveVolumeRun,
  completeDestructiveVolumeAttempt,
  establishOrConfirmDestructiveVolumeTimeout,
  finalizeUnassociatedDestructiveVolumeAttempt,
  recordDestructiveVolumeEvent,
} from "#/modules/operations/destructive-volume-attempt.repository";
import {
  destructiveVolumeRequestedEventName,
  destructiveVolumeRequestedEventType,
} from "#/modules/operations/destructive-volume-outbox";
import {
  recoverDestructiveVolumeAcceptance,
  submitDestructiveVolume,
  verifyFreshDestructiveVolumeEvidence,
  watchDestructiveVolumeBatch,
} from "#/modules/operations/destructive-volume-runtime-adapter.server";
import {
  closeDestructiveVolumeWatchAndReconcile,
  loadDestructiveVolumeWorkflowContext,
} from "#/modules/operations/destructive-volume-workflow.server";
import {
  DestructiveVolumePersistenceFailure,
  DestructiveVolumeProviderFailure,
} from "#/modules/operations/destructive-volume-errors";
import { runInngestEffect } from "#/server/run.server";

export const PROCESS_DESTRUCTIVE_VOLUME_FUNCTION_ID = "process-destructive-volume";
const DESTRUCTIVE_VOLUME_POLL_INTERVAL = "5s";
const DESTRUCTIVE_VOLUME_MAX_ITERATIONS = 120;
const DESTRUCTIVE_VOLUME_PAGES_PER_STEP = 10;
const DestructiveVolumeRequestedEnvelope = Schema.Struct({
  ...inngestEventEnvelopeFields,
  name: Schema.Literal(destructiveVolumeRequestedEventName),
  data: Schema.Struct({
    attemptId: Schema.Trim.check(Schema.isMinLength(1)),
  }),
});
const DestructiveVolumeFailureEnvelope = inngestFunctionFailedEnvelopeSchema(
  DestructiveVolumeRequestedEnvelope,
);

export const decodeDestructiveVolumeFailureEvent = Schema.decodeUnknownOption(
  DestructiveVolumeFailureEnvelope,
);

type FailureStepTools = Pick<PloyzStepTools, "run">;

async function handleFailure(input: {
  attemptId: string;
  inngestRunId: string;
  step: FailureStepTools;
}) {
  const context = await input.step.run(
    "load-destructive-volume-failure-context",
    () =>
      runInngestEffect(
        loadDestructiveVolumeWorkflowContext({ attemptId: input.attemptId }),
      ),
  );
  if (!context || context.attempt.state === "terminal") return;
  const claimed = await input.step.run(
    "claim-destructive-volume-failure-run",
    () =>
      runInngestEffect(
        claimDestructiveVolumeRun({
          attemptId: input.attemptId,
          inngestRunId: input.inngestRunId,
        }),
      ),
  );
  const owned = projectDestructiveVolumeWorkflowAttempt(claimed.attempt);
  if (owned.state === "owned_associated") {
    await input.step.run("close-destructive-volume-failure-watch", () =>
      runInngestEffect(
        closeDestructiveVolumeWatchAndReconcile({
          attemptId: input.attemptId,
          organizationId: context.organizationId,
          operationId: owned.operationId,
          expectedInngestRunId: input.inngestRunId,
          state: "cloud_timeout",
        }),
      ),
    );
    return;
  }
  if (owned.state !== "owned_unassociated") return;
  const recovery = await input.step.run(
    "recover-destructive-volume-acceptance",
    async () => {
      try {
        return {
          kind: "recovered" as const,
          recovered: await runInngestEffect(
            recoverDestructiveVolumeAcceptance({
              attemptId: input.attemptId,
              target: owned.target,
            }),
          ),
        };
      } catch (cause) {
        if (!(cause instanceof DestructiveVolumeProviderFailure)) throw cause;
        return { kind: "uncertain" as const };
      }
    },
  );
  if (recovery.kind === "recovered" && recovery.recovered.state === "accepted") {
    const accepted = recovery.recovered;
    const attached = await input.step.run(
      "attach-destructive-volume-failure-operation",
      async () => {
        try {
          await runInngestEffect(
            attachDestructiveVolumeOperation({
              organizationId: context.organizationId,
              attemptId: input.attemptId,
              operationId: accepted.operation_id,
              startSequence: accepted.start_sequence,
              inngestRunId: input.inngestRunId,
            }),
          );
          return { kind: "attached" as const };
        } catch (cause) {
          if (!(cause instanceof DestructiveVolumePersistenceFailure)) {
            throw cause;
          }
          return { kind: "persist_failed" as const };
        }
      },
    );
    if (attached.kind === "persist_failed") {
      await input.step.run("establish-destructive-volume-failure-timeout", () =>
        runInngestEffect(
          establishOrConfirmDestructiveVolumeTimeout({
            attemptId: input.attemptId,
            organizationId: context.organizationId,
            operationId: accepted.operation_id,
            startSequence: accepted.start_sequence,
            expectedInngestRunId: input.inngestRunId,
          }),
        ),
      );
      return;
    }
    await input.step.run("close-destructive-volume-recovered-watch", () =>
      runInngestEffect(
        closeDestructiveVolumeWatchAndReconcile({
          attemptId: input.attemptId,
          organizationId: context.organizationId,
          operationId: accepted.operation_id,
          expectedInngestRunId: input.inngestRunId,
          state: "cloud_timeout",
        }),
      ),
    );
    return;
  }
  await input.step.run("finalize-unassociated-destructive-volume-failure", () =>
    runInngestEffect(
      finalizeUnassociatedDestructiveVolumeAttempt({
        attemptId: input.attemptId,
        organizationId: context.organizationId,
        expectedInngestRunId: input.inngestRunId,
        event: "submission_failed",
        failureCode:
          recovery.kind === "uncertain" ? "recovery_uncertain" : undefined,
        message:
          recovery.kind === "uncertain"
            ? "Core recovery could not confirm whether destructive volume submission was accepted."
            : "Destructive volume workflow exhausted retries before Core association.",
      }),
    ),
  );
}

export const createProcessDestructiveVolume = (inngest: PloyzInngest) =>
  inngest.createFunction(
    {
      id: PROCESS_DESTRUCTIVE_VOLUME_FUNCTION_ID,
      retries: 5,
      triggers: [{ event: destructiveVolumeRequestedEventType }],
      concurrency: [{ key: "event.data.attemptId", limit: 1 }],
      onFailure: async ({ event, step }) => {
        const decoded = decodeDestructiveVolumeFailureEvent(event);
        if (Option.isNone(decoded)) return;
        await handleFailure({
          attemptId: decoded.value.data.event.data.attemptId,
          inngestRunId: decoded.value.data.run_id,
          step,
        });
      },
    },
    async ({ event, step, runId }) => {
      const attemptId = await step.run(
        "normalize-destructive-volume-attempt-id",
        () =>
          decodeInngestEnvelope(DestructiveVolumeRequestedEnvelope)(event).data
            .attemptId,
      );
      const context = await step.run(
        "load-destructive-volume-context",
        () =>
          runInngestEffect(
            loadDestructiveVolumeWorkflowContext({ attemptId }),
          ),
      );
      if (!context) return { attemptId, status: "missing", skipped: true };
      if (context.attempt.state === "terminal") {
        return { attemptId, status: context.attempt.disposition, skipped: true };
      }
      const claimed = await step.run("claim-destructive-volume-run", async () => {
        const result = await runInngestEffect(
          claimDestructiveVolumeRun({ attemptId, inngestRunId: runId }),
        );
        const attempt = projectDestructiveVolumeWorkflowAttempt(
          result.attempt,
        );
        if (
          (attempt.state !== "owned_unassociated" &&
            attempt.state !== "owned_associated") ||
          attempt.inngestRunId !== runId
        ) {
          throw new NonRetriableError(
            "Destructive volume claim did not establish workflow ownership.",
          );
        }
        return { attempt };
      });
      const owned = claimed.attempt;
      let operationId: string;
      if (owned.state === "owned_unassociated") {
        const freshness = await step.run(
          "verify-fresh-destructive-volume-evidence",
          () =>
            runInngestEffect(
              verifyFreshDestructiveVolumeEvidence({
                target: owned.target,
                evidence: owned.evidence,
              }),
            ),
        );
        if (freshness.state === "changed") {
          await step.run("fail-destructive-volume-evidence-changed", () =>
            runInngestEffect(
              finalizeUnassociatedDestructiveVolumeAttempt({
                attemptId,
                organizationId: context.organizationId,
                expectedInngestRunId: runId,
                event: "submission_failed",
                failureCode: "evidence_changed",
                message: freshness.message,
              }),
            ),
          );
          return { attemptId, status: "evidence_changed" };
        }
        const accepted = await step.run("submit-destructive-volume", () =>
          runInngestEffect(
            submitDestructiveVolume({ attemptId, target: owned.target }),
          ),
        );
        operationId = accepted.operation_id;
        await step.run("attach-destructive-volume-operation", () =>
          runInngestEffect(
            attachDestructiveVolumeOperation({
              organizationId: context.organizationId,
              attemptId,
              operationId: accepted.operation_id,
              startSequence: accepted.start_sequence,
              inngestRunId: runId,
            }),
          ),
        );
      } else {
        operationId = owned.operationId;
      }

      for (
        let iteration = 0;
        iteration < DESTRUCTIVE_VOLUME_MAX_ITERATIONS;
        iteration += 1
      ) {
        const observation = await step.run(
          `watch-destructive-volume-${iteration}`,
          async () => ({
            watched: await runInngestEffect(
              watchDestructiveVolumeBatch({
                organizationId: context.organizationId,
                operationId,
                maxPages: DESTRUCTIVE_VOLUME_PAGES_PER_STEP,
              }),
            ),
            observedAt: new Date().toISOString(),
          }),
        );
        const terminal = [...observation.watched.events]
          .reverse()
          .find(isDestructiveVolumeTerminalEvidenceEvent);
        if (terminal) {
          await step.run(`record-destructive-volume-terminal-${iteration}`, () =>
            terminal.event === "volume_remove_completed"
              ? runInngestEffect(
                  completeDestructiveVolumeAttempt({ attemptId, operationId }),
                )
              : runInngestEffect(
                  recordDestructiveVolumeEvent({
                    attemptId,
                    event: projectDestructiveVolumeTerminalAttemptEvent(
                      operationId,
                      terminal,
                    ),
                  }),
                ),
          );
          return { attemptId, status: terminal.event };
        }
        if (observation.watched.state === "terminal") {
          throw new Error(
            "Core volume removal became terminal without typed terminal evidence.",
          );
        }
        if (
          new Date(observation.observedAt).getTime() >=
          new Date(owned.deadlineAt).getTime()
        ) {
          const closed = await step.run("close-destructive-volume-deadline", () =>
            runInngestEffect(
              closeDestructiveVolumeWatchAndReconcile({
                attemptId,
                organizationId: context.organizationId,
                operationId,
                expectedInngestRunId: runId,
                state: "cloud_timeout",
              }),
            ),
          );
          return { attemptId, status: closed.state };
        }
        await step.sleep(
          `wait-for-destructive-volume-${iteration}`,
          DESTRUCTIVE_VOLUME_POLL_INTERVAL,
        );
      }
      const closed = await step.run("close-destructive-volume-budget", () =>
        runInngestEffect(
          closeDestructiveVolumeWatchAndReconcile({
            attemptId,
            organizationId: context.organizationId,
            operationId,
            expectedInngestRunId: runId,
            state: "cloud_timeout",
          }),
        ),
      );
      return { attemptId, status: closed.state };
    },
  );
