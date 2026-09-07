import { and, asc, eq, lt } from "drizzle-orm";
import { Effect } from "effect";
import { isValidGithubId } from "#/modules/github/github-ingestion.contracts";
import type {
  GithubPendingCheckSuiteTransition,
  GithubPendingEnvironmentTrigger,
} from "#/modules/github/github-ingestion.repository.types";
import { repositoryError } from "#/modules/github/github-ingestion.repository.types";
import { Database } from "#/server/database.server";
import {
  githubEnvironmentTrigger as schemaGithubEnvironmentTrigger,
  githubCheckSuiteProjection as schemaGithubCheckSuiteProjection,
} from "#/modules/github/tables";

function validLimit(limit: number) {
  return Number.isSafeInteger(limit) && limit >= 1 && limit <= 100;
}

function environmentTrigger(
  row: typeof schemaGithubEnvironmentTrigger.$inferSelect,
): GithubPendingEnvironmentTrigger {
  // SAFETY: non-paths rows persist an all_services reason; the DB column is the wider GithubTriggerReason union.
  return {
    triggerId: row.id,
    installationId: row.installationId,
    repositoryId: row.repositoryId,
    ref: row.ref,
    headSha: row.headSha,
    environmentId: row.environmentId,
    serviceIds: row.serviceIds,
    selection:
      row.selectionMode === "paths"
        ? { mode: "paths", reason: "changed_paths" }
        : {
            mode: "all_services",
            reason: row.reason as Extract<
              GithubPendingEnvironmentTrigger["selection"],
              { mode: "all_services" }
            >["reason"],
          },
    sourceDeliveryId: row.sourceDeliveryId,
    sourceReceiptSequence: row.sourceReceiptSequence,
    triggerRevision: row.triggerRevision,
  };
}

function checkTransition(
  row: typeof schemaGithubCheckSuiteProjection.$inferSelect,
): GithubPendingCheckSuiteTransition {
  return {
    installationId: row.installationId,
    repositoryId: row.repositoryId,
    checkSuiteId: row.checkSuiteId,
    headSha: row.headSha,
    status: row.status,
    conclusion: row.conclusion,
    sourceUpdatedAt: row.sourceUpdatedAt,
    sourceDeliveryId: row.lastDeliveryId,
    sourceReceiptSequence: row.lastReceiptSequence,
    transitionRevision: row.transitionRevision,
  };
}

export const listPendingGithubEnvironmentTriggers = Effect.fn(
  "Github.listPendingEnvironmentTriggers",
)(function* (input: { limit: number }) {
  if (!validLimit(input.limit)) {
    return yield* repositoryError("invalid_input", false);
  }
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select()
    .from(schemaGithubEnvironmentTrigger)
    .where(
      and(
        eq(schemaGithubEnvironmentTrigger.publishState, "pending"),
        lt(
          schemaGithubEnvironmentTrigger.publishedRevision,
          schemaGithubEnvironmentTrigger.triggerRevision,
        ),
      ),
    )
    .orderBy(
      asc(schemaGithubEnvironmentTrigger.createdAt),
      asc(schemaGithubEnvironmentTrigger.id),
    )
    .limit(input.limit);
  return rows.map(environmentTrigger);
});

export const acknowledgeGithubEnvironmentTrigger = Effect.fn(
  "Github.acknowledgeEnvironmentTrigger",
)(function* (input: {
  triggerId: string;
  triggerRevision: number;
  publishedAt: Date;
}) {
  if (
    !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(
      input.triggerId,
    ) ||
    !isValidGithubId(input.triggerRevision) ||
    !(input.publishedAt instanceof Date) ||
    !Number.isFinite(input.publishedAt.getTime())
  ) {
    return yield* repositoryError("invalid_input", false);
  }
  const { drizzle } = yield* Database;
  const updated = yield* drizzle
    .update(schemaGithubEnvironmentTrigger)
    .set({
      publishedRevision: input.triggerRevision,
      publishState: "published",
      publishedAt: input.publishedAt,
      updatedAt: new Date(),
    })
    .where(
      and(
        eq(schemaGithubEnvironmentTrigger.id, input.triggerId),
        eq(
          schemaGithubEnvironmentTrigger.triggerRevision,
          input.triggerRevision,
        ),
        lt(
          schemaGithubEnvironmentTrigger.publishedRevision,
          input.triggerRevision,
        ),
        eq(schemaGithubEnvironmentTrigger.publishState, "pending"),
      ),
    )
    .returning({ id: schemaGithubEnvironmentTrigger.id });
  return { acknowledged: updated.length === 1 };
});

export const listPendingGithubCheckSuiteTransitions = Effect.fn(
  "Github.listPendingCheckSuiteTransitions",
)(function* (input: { limit: number }) {
  if (!validLimit(input.limit)) {
    return yield* repositoryError("invalid_input", false);
  }
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select()
    .from(schemaGithubCheckSuiteProjection)
    .where(
      lt(
        schemaGithubCheckSuiteProjection.publishedRevision,
        schemaGithubCheckSuiteProjection.transitionRevision,
      ),
    )
    .orderBy(
      asc(schemaGithubCheckSuiteProjection.updatedAt),
      asc(schemaGithubCheckSuiteProjection.installationId),
      asc(schemaGithubCheckSuiteProjection.repositoryId),
      asc(schemaGithubCheckSuiteProjection.checkSuiteId),
    )
    .limit(input.limit);
  return rows.map(checkTransition);
});

export const loadPendingGithubCheckSuiteTransition = Effect.fn(
  "Github.loadPendingCheckSuiteTransition",
)(function* (input: {
  installationId: number;
  repositoryId: number;
  checkSuiteId: number;
}) {
  if (
    !isValidGithubId(input.installationId) ||
    !isValidGithubId(input.repositoryId) ||
    !isValidGithubId(input.checkSuiteId)
  ) {
    return yield* repositoryError("invalid_input", false);
  }
  const { drizzle } = yield* Database;
  const [row] = yield* drizzle
    .select()
    .from(schemaGithubCheckSuiteProjection)
    .where(
      and(
        eq(
          schemaGithubCheckSuiteProjection.installationId,
          input.installationId,
        ),
        eq(schemaGithubCheckSuiteProjection.repositoryId, input.repositoryId),
        eq(schemaGithubCheckSuiteProjection.checkSuiteId, input.checkSuiteId),
        lt(
          schemaGithubCheckSuiteProjection.publishedRevision,
          schemaGithubCheckSuiteProjection.transitionRevision,
        ),
      ),
    )
    .limit(1);
  return row ? checkTransition(row) : null;
});

export const acknowledgeGithubCheckSuiteTransition = Effect.fn(
  "Github.acknowledgeCheckSuiteTransition",
)(function* (input: {
  installationId: number;
  repositoryId: number;
  checkSuiteId: number;
  transitionRevision: number;
}) {
  if (
    !isValidGithubId(input.installationId) ||
    !isValidGithubId(input.repositoryId) ||
    !isValidGithubId(input.checkSuiteId) ||
    !isValidGithubId(input.transitionRevision)
  ) {
    return yield* repositoryError("invalid_input", false);
  }
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .update(schemaGithubCheckSuiteProjection)
    .set({
      publishedRevision: input.transitionRevision,
      updatedAt: new Date(),
    })
    .where(
      and(
        eq(
          schemaGithubCheckSuiteProjection.installationId,
          input.installationId,
        ),
        eq(schemaGithubCheckSuiteProjection.repositoryId, input.repositoryId),
        eq(schemaGithubCheckSuiteProjection.checkSuiteId, input.checkSuiteId),
        eq(
          schemaGithubCheckSuiteProjection.transitionRevision,
          input.transitionRevision,
        ),
        lt(
          schemaGithubCheckSuiteProjection.publishedRevision,
          input.transitionRevision,
        ),
      ),
    )
    .returning({
      checkSuiteId: schemaGithubCheckSuiteProjection.checkSuiteId,
    });
  return { acknowledged: rows.length === 1 };
});
