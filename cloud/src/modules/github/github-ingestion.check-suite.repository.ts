import { and, eq, sql } from "drizzle-orm";
import { Effect } from "effect";
import {
  GITHUB_CHECK_SUITE_CONCLUSIONS,
  GITHUB_CHECK_SUITE_STATUSES,
  isValidGithubExactSha,
  isValidGithubId,
} from "#/modules/github/github-ingestion.contracts";
import { completeGithubDelivery } from "#/modules/github/github-ingestion.delivery.repository";
import { withGithubTransaction } from "#/modules/github/github-ingestion.transaction";
import type { ApplyGithubCheckSuiteTestimonyInput } from "#/modules/github/github-ingestion.repository.types";
import { Database } from "#/server/database.server";
import { repositoryError } from "#/modules/github/github-ingestion.repository.types";
import {
  githubCheckSuiteProjection as schemaGithubCheckSuiteProjection,
  type GithubWebhookOutcome,
} from "#/modules/github/tables";

type Projection = typeof schemaGithubCheckSuiteProjection.$inferSelect;

function validInput(input: ApplyGithubCheckSuiteTestimonyInput) {
  return (
    /^[A-Za-z0-9][A-Za-z0-9._:-]{0,254}$/.test(input.deliveryId) &&
    isValidGithubId(input.receiptSequence) &&
    /^[A-Za-z0-9][A-Za-z0-9._:-]{0,254}$/.test(input.processingRunId) &&
    isValidGithubId(input.installationId) &&
    isValidGithubId(input.repositoryId) &&
    isValidGithubId(input.checkSuiteId) &&
    isValidGithubExactSha(input.headSha) &&
    GITHUB_CHECK_SUITE_STATUSES.includes(input.status) &&
    (input.conclusion === null ||
      GITHUB_CHECK_SUITE_CONCLUSIONS.includes(input.conclusion)) &&
    input.sourceUpdatedAt instanceof Date &&
    Number.isFinite(input.sourceUpdatedAt.getTime())
  );
}

function compareOrder(
  input: ApplyGithubCheckSuiteTestimonyInput,
  row: Projection,
) {
  const time = input.sourceUpdatedAt.getTime() - row.sourceUpdatedAt.getTime();
  if (time !== 0) return time;
  const sequence = input.receiptSequence - row.lastReceiptSequence;
  if (sequence !== 0) return sequence;
  return input.deliveryId.localeCompare(row.lastDeliveryId);
}

function sameState(
  input: ApplyGithubCheckSuiteTestimonyInput,
  row: Projection,
) {
  return (
    input.headSha === row.headSha &&
    input.status === row.status &&
    input.conclusion === row.conclusion
  );
}

export const applyGithubCheckSuiteTestimony = Effect.fn(
  "Github.applyCheckSuiteTestimony",
)(function* (input: ApplyGithubCheckSuiteTestimonyInput) {
  if (!validInput(input)) {
    return yield* repositoryError("invalid_input", false);
  }
  return yield* withGithubTransaction(
    Effect.gen(function* () {
      const { drizzle } = yield* Database;
      const authorityKey = `${input.installationId}:${input.repositoryId}:${input.checkSuiteId}`;
      yield* drizzle.execute(
        sql`select pg_advisory_xact_lock(hashtextextended(${authorityKey}, 111))`,
      );
      const [existing] = yield* drizzle
        .select()
        .from(schemaGithubCheckSuiteProjection)
        .where(
          and(
            eq(
              schemaGithubCheckSuiteProjection.installationId,
              input.installationId,
            ),
            eq(
              schemaGithubCheckSuiteProjection.repositoryId,
              input.repositoryId,
            ),
            eq(
              schemaGithubCheckSuiteProjection.checkSuiteId,
              input.checkSuiteId,
            ),
          ),
        )
        .for("update");
      if (existing && existing.publishedRevision < existing.transitionRevision) {
        return yield* repositoryError("pending_publication", true);
      }
      const complete = (outcome: GithubWebhookOutcome) =>
        completeGithubDelivery(
          {
            deliveryId: input.deliveryId,
            receiptSequence: input.receiptSequence,
            processingRunId: input.processingRunId,
            identity: { ...input, eventKind: "check_suite" },
          },
          outcome,
        );
      if (existing && compareOrder(input, existing) < 0) {
        yield* complete("ignored_stale");
        return {
          disposition: "stale" as const,
          transitionRevision: existing.transitionRevision,
        };
      }
      if (existing && sameState(input, existing)) {
        const [updated] = yield* drizzle
          .update(schemaGithubCheckSuiteProjection)
          .set({
            sourceUpdatedAt: input.sourceUpdatedAt,
            lastDeliveryId: input.deliveryId,
            lastReceiptSequence: input.receiptSequence,
            updatedAt: new Date(),
          })
          .where(
            and(
              eq(
                schemaGithubCheckSuiteProjection.installationId,
                input.installationId,
              ),
              eq(
                schemaGithubCheckSuiteProjection.repositoryId,
                input.repositoryId,
              ),
              eq(
                schemaGithubCheckSuiteProjection.checkSuiteId,
                input.checkSuiteId,
              ),
              eq(
                schemaGithubCheckSuiteProjection.transitionRevision,
                existing.transitionRevision,
              ),
              eq(
                schemaGithubCheckSuiteProjection.publishedRevision,
                existing.transitionRevision,
              ),
            ),
          )
          .returning({
            transitionRevision:
              schemaGithubCheckSuiteProjection.transitionRevision,
          });
        if (!updated) {
          return yield* repositoryError("pending_publication", true);
        }
        yield* complete("check_suite_unchanged");
        return {
          disposition: "unchanged" as const,
          transitionRevision: updated.transitionRevision,
        };
      }

      const transitionRevision = (existing?.transitionRevision ?? 0) + 1;
      const values = {
        installationId: input.installationId,
        repositoryId: input.repositoryId,
        checkSuiteId: input.checkSuiteId,
        headSha: input.headSha,
        status: input.status,
        conclusion: input.conclusion,
        sourceUpdatedAt: input.sourceUpdatedAt,
        lastDeliveryId: input.deliveryId,
        lastReceiptSequence: input.receiptSequence,
        transitionRevision,
        updatedAt: new Date(),
      } as const;
      const projectedRows = existing
        ? yield* drizzle
            .update(schemaGithubCheckSuiteProjection)
            .set(values)
            .where(
              and(
                eq(
                  schemaGithubCheckSuiteProjection.installationId,
                  input.installationId,
                ),
                eq(
                  schemaGithubCheckSuiteProjection.repositoryId,
                  input.repositoryId,
                ),
                eq(
                  schemaGithubCheckSuiteProjection.checkSuiteId,
                  input.checkSuiteId,
                ),
                eq(
                  schemaGithubCheckSuiteProjection.transitionRevision,
                  existing.transitionRevision,
                ),
                eq(
                  schemaGithubCheckSuiteProjection.publishedRevision,
                  existing.transitionRevision,
                ),
              ),
            )
            .returning({
              transitionRevision:
                schemaGithubCheckSuiteProjection.transitionRevision,
            })
        : yield* drizzle
            .insert(schemaGithubCheckSuiteProjection)
            .values(values)
            .onConflictDoNothing()
            .returning({
              transitionRevision:
                schemaGithubCheckSuiteProjection.transitionRevision,
            });
      const projected = projectedRows[0];
      if (!projected) {
        return yield* repositoryError("pending_publication", true);
      }
      yield* complete("check_suite_projected");
      return { disposition: "applied" as const, transitionRevision };
    }),
  );
});
