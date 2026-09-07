import { Schema } from "effect";
import type { PloyzInngest } from "#/modules/inngest/client";
import { projectDestructiveVolumeWorkflowAttempt } from "#/modules/operations/destructive-volume-attempt";
import {
  establishOrConfirmDestructiveVolumeTimeout,
  finalizeUnassociatedDestructiveVolumeAttempt,
  listOwnedDestructiveVolumeAttemptsPage,
  type RawOwnedDestructiveVolumeAttempt,
} from "#/modules/operations/destructive-volume-attempt.repository";
import { closeDestructiveVolumeWatchAndReconcile } from "#/modules/operations/destructive-volume-workflow.server";
import { runInngestEffect } from "#/server/run.server";

export const DESTRUCTIVE_VOLUME_RECOVERY_LIMIT = 50;
export const DESTRUCTIVE_VOLUME_ABANDONED_GRACE_MS = 7 * 24 * 60 * 60_000;

const DurableDate = Schema.Union([Schema.Date, Schema.DateFromString]);
const DurableNullableDate = Schema.NullOr(DurableDate);
const AbandonedOwnedAttemptPage = Schema.Array(
  Schema.Struct({
    organizationId: Schema.String,
    deadlineAt: DurableDate,
    attempt: Schema.Struct({
      requestPublishedAt: DurableNullableDate,
      acceptedAt: DurableNullableDate,
      deadlineAt: DurableNullableDate,
      terminalAt: DurableNullableDate,
      createdAt: DurableDate,
      updatedAt: DurableDate,
    }),
  }),
);

function decodeAbandonedOwnedAttempts<Page>(
  pageResult: Page,
): RawOwnedDestructiveVolumeAttempt[] {
  // SAFETY: preserve keeps runtime-only attempt fields that Jsonify may omit from the date schema.
  return Schema.decodeUnknownSync(AbandonedOwnedAttemptPage)(pageResult, {
    onExcessProperty: "preserve",
  }) as RawOwnedDestructiveVolumeAttempt[];
}

export const createRecoverAbandonedDestructiveVolumeAttempts = (
  inngest: PloyzInngest,
) =>
  inngest.createFunction(
    {
      id: "recover-abandoned-destructive-volume-attempts",
      retries: 3,
      triggers: [{ cron: "* * * * *" }],
      concurrency: [{ limit: 1 }],
    },
    async ({ step }) => {
      const deadlineAtOrBefore = new Date(
        Date.now() - DESTRUCTIVE_VOLUME_ABANDONED_GRACE_MS,
      );
      let after: { deadlineAt: Date; id: string } | undefined;
      let recovered = 0;
      let firstFailure: unknown;

      while (true) {
        const pageResult = await step.run(
          `list-abandoned-destructive-volume-${after?.id ?? "first"}`,
          () =>
            runInngestEffect(
              listOwnedDestructiveVolumeAttemptsPage(
                after
                  ? {
                      deadlineAtOrBefore,
                      after,
                      limit: DESTRUCTIVE_VOLUME_RECOVERY_LIMIT,
                    }
                  : {
                      deadlineAtOrBefore,
                      limit: DESTRUCTIVE_VOLUME_RECOVERY_LIMIT,
                    },
              ),
            ),
        );
        const page = decodeAbandonedOwnedAttempts(pageResult);
        if (page.length === 0) break;

        const settled = await Promise.allSettled(
          page.map(async (candidate) => {
            const attempt = projectDestructiveVolumeWorkflowAttempt(
              candidate.attempt,
            );
            if (attempt.state === "owned_unassociated") {
              await step.run(
                `fail-stale-destructive-volume-${attempt.id}`,
                () =>
                  runInngestEffect(
                    finalizeUnassociatedDestructiveVolumeAttempt({
                      attemptId: attempt.id,
                      organizationId: candidate.organizationId,
                      expectedInngestRunId: attempt.inngestRunId,
                      event: "submission_failed",
                      failureCode: "deadline_exceeded",
                      message:
                        "Destructive volume workflow remained unassociated beyond its durable deadline.",
                      now: new Date(),
                    }),
                  ),
              );
              return;
            }
            if (attempt.state !== "owned_associated") {
              throw new Error(
                "Abandoned destructive volume recovery candidate is not durably owned.",
              );
            }
            try {
              await step.run(
                `close-stale-destructive-volume-${attempt.id}`,
                () =>
                  runInngestEffect(
                    closeDestructiveVolumeWatchAndReconcile({
                      attemptId: attempt.id,
                      organizationId: candidate.organizationId,
                      operationId: attempt.operationId,
                      expectedInngestRunId: attempt.inngestRunId,
                      state: "cloud_timeout",
                      now: new Date(),
                    }),
                  ),
              );
            } catch {
              await step.run(
                `confirm-stale-destructive-volume-${attempt.id}`,
                () =>
                  runInngestEffect(
                    establishOrConfirmDestructiveVolumeTimeout({
                      attemptId: attempt.id,
                      organizationId: candidate.organizationId,
                      operationId: attempt.operationId,
                      startSequence: attempt.startSequence,
                      expectedInngestRunId: attempt.inngestRunId,
                      now: new Date(),
                    }),
                  ),
              );
            }
          }),
        );
        recovered += settled.filter(
          (result) => result.status === "fulfilled",
        ).length;
        firstFailure ??= settled.find(
          (result): result is PromiseRejectedResult =>
            result.status === "rejected",
        )?.reason;

        const last = page.at(-1);
        if (!last || page.length < DESTRUCTIVE_VOLUME_RECOVERY_LIMIT) break;
        const cursorDeadline = new Date(last.deadlineAt);
        if (!Number.isFinite(cursorDeadline.getTime())) {
          throw new Error(
            "Abandoned destructive volume cursor deadline is invalid.",
          );
        }
        after = { deadlineAt: cursorDeadline, id: last.attempt.id };
      }

      if (firstFailure !== undefined) throw firstFailure;
      return { recovered };
    },
  );
