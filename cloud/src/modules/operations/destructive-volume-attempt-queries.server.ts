import "@tanstack/react-start/server-only";
import {
  and,
  asc,
  desc,
  eq,
  gt,
  inArray,
  isNotNull,
  isNull,
  lte,
  or,
} from "drizzle-orm";
import { alias } from "drizzle-orm/pg-core";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import {
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
} from "#/modules/operations/tables";
import { environment as schemaEnvironment } from "#/modules/project/tables";
import { DestructiveVolumeConflict } from "#/modules/operations/destructive-volume-errors";
import { Effect } from "effect";
import { Database, type DatabaseService } from "#/server/database.server";
import {
  type DestructiveVolumeAttemptRecord,
  destructiveVolumeRepositoryError,
} from "#/modules/operations/destructive-volume-attempt-persistence.server";

const destructiveVolumeRetrySource = alias(
  schemaDestructiveVolumeAttempt,
  "destructive_volume_retry_source",
);

export type RawOwnedDestructiveVolumeAttempt = {
  attempt: DestructiveVolumeAttemptRecord;
  organizationId: string;
  deadlineAt: Date | string;
};

export function unpublishedDestructiveVolumeAttemptFilter() {
  return and(
    isNull(schemaDestructiveVolumeAttempt.requestPublishedAt),
    isNull(schemaDestructiveVolumeAttempt.inngestRunId),
    eq(schemaEnvironmentDeployment.status, "applied"),
    or(
      and(
        eq(schemaDestructiveVolumeAttempt.disposition, "active"),
        isNull(schemaDestructiveVolumeAttempt.operationId),
      ),
      and(
        eq(schemaDestructiveVolumeAttempt.disposition, "accepted"),
        isNotNull(schemaDestructiveVolumeAttempt.operationId),
        isNotNull(schemaDestructiveVolumeAttempt.startSequence),
        isNotNull(schemaDestructiveVolumeAttempt.retryOfAttemptId),
        inArray(destructiveVolumeRetrySource.disposition, [
          "cloud_timeout",
          "cloud_cancelled",
        ]),
        eq(
          destructiveVolumeRetrySource.operationId,
          schemaDestructiveVolumeAttempt.operationId,
        ),
        eq(
          destructiveVolumeRetrySource.startSequence,
          schemaDestructiveVolumeAttempt.startSequence,
        ),
      ),
    ),
  );
}

export function releaseDestructiveVolumeAttemptsForAppliedDeploymentInTransaction(
  tx: DatabaseService["drizzle"],
  environmentDeploymentId: string,
) {
  return Effect.gen(function* () {
    const [deployment] = yield* tx
      .select({ status: schemaEnvironmentDeployment.status })
      .from(schemaEnvironmentDeployment)
      .where(eq(schemaEnvironmentDeployment.id, environmentDeploymentId))
      .limit(1);
    if (deployment?.status !== "applied") {
      return yield* new DestructiveVolumeConflict({
        message:
          "Destructive volume attempts can be released only by their applied deployment.",
      });
    }
    return yield* tx
      .select()
      .from(schemaDestructiveVolumeAttempt)
      .where(
        and(
          eq(
            schemaDestructiveVolumeAttempt.environmentDeploymentId,
            environmentDeploymentId,
          ),
          eq(schemaDestructiveVolumeAttempt.disposition, "active"),
          isNull(schemaDestructiveVolumeAttempt.operationId),
          isNull(schemaDestructiveVolumeAttempt.inngestRunId),
          isNull(schemaDestructiveVolumeAttempt.requestPublishedAt),
        ),
      );
  });
}

export function failUnsubmittedDestructiveVolumeAttemptsForDeploymentInTransaction(
  tx: DatabaseService["drizzle"],
  input: {
    environmentDeploymentId: string;
    deploymentDisposition: "failed" | "cancelled";
    now: Date;
  },
) {
  return Effect.gen(function* () {
    const [deployment] = yield* tx
      .select({ status: schemaEnvironmentDeployment.status })
      .from(schemaEnvironmentDeployment)
      .where(
        eq(
          schemaEnvironmentDeployment.id,
          input.environmentDeploymentId,
        ),
      )
      .limit(1);
    if (deployment?.status !== input.deploymentDisposition) {
      return yield* new DestructiveVolumeConflict({
        message:
          "Destructive volume deployment terminalization conflicts with deployment state.",
      });
    }
    const event = {
      event: "deployment_not_applied",
      deploymentDisposition: input.deploymentDisposition,
    } as const;
    return yield* tx
      .update(schemaDestructiveVolumeAttempt)
      .set({
        disposition: "failed",
        terminalEvent: event,
        failure: {
          code: "deployment_not_applied",
          message: `Deployment ${input.deploymentDisposition} before destructive volume submission.`,
        },
        terminalAt: input.now,
        updatedAt: input.now,
      })
      .where(
        and(
          eq(
            schemaDestructiveVolumeAttempt.environmentDeploymentId,
            input.environmentDeploymentId,
          ),
          eq(schemaDestructiveVolumeAttempt.disposition, "active"),
          isNull(schemaDestructiveVolumeAttempt.operationId),
          isNull(schemaDestructiveVolumeAttempt.inngestRunId),
        ),
      )
      .returning();
  });
}

export function acknowledgeDestructiveVolumeRequest(
  input: {
    attemptId: string;
    publishedAt: Date;
  },
) {
  return Effect.gen(function* () {
    const database = yield* Database;
    const [updated] = yield* database.drizzle
      .update(schemaDestructiveVolumeAttempt)
      .set({
        requestPublishedAt: input.publishedAt,
        updatedAt: input.publishedAt,
      })
      .where(
        and(
          eq(schemaDestructiveVolumeAttempt.id, input.attemptId),
          isNull(schemaDestructiveVolumeAttempt.requestPublishedAt),
        ),
      )
      .returning({ id: schemaDestructiveVolumeAttempt.id });
    return { acknowledged: Boolean(updated) };
  }).pipe(
    Effect.mapError(
      destructiveVolumeRepositoryError,
    ),
  );
}

export function listUnpublishedDestructiveVolumeAttempts(limit = 50) {
  return Effect.gen(function* () {
    const database = yield* Database;
    return yield* database.drizzle
      .select({ id: schemaDestructiveVolumeAttempt.id })
      .from(schemaDestructiveVolumeAttempt)
      .innerJoin(
        schemaEnvironmentDeployment,
        eq(
          schemaEnvironmentDeployment.id,
          schemaDestructiveVolumeAttempt.environmentDeploymentId,
        ),
      )
      .leftJoin(
        destructiveVolumeRetrySource,
        eq(
          destructiveVolumeRetrySource.id,
          schemaDestructiveVolumeAttempt.retryOfAttemptId,
        ),
      )
      .where(unpublishedDestructiveVolumeAttemptFilter())
      .orderBy(asc(schemaDestructiveVolumeAttempt.createdAt))
      .limit(limit);
  }).pipe(
    Effect.mapError(destructiveVolumeRepositoryError),
  );
}

export function listOwnedDestructiveVolumeAttemptsPage(
  input: {
    deadlineAtOrBefore: Date;
    after?: { deadlineAt: Date; id: string };
    limit?: number;
  },
) {
  const limit = Math.min(Math.max(input.limit ?? 50, 1), 50);
  return Effect.gen(function* () {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select({
        attempt: schemaDestructiveVolumeAttempt,
        organizationId: schemaEnvironment.organizationId,
        deadlineAt: schemaDestructiveVolumeAttempt.deadlineAt,
      })
      .from(schemaDestructiveVolumeAttempt)
      .innerJoin(
        schemaEnvironmentDeployment,
        eq(
          schemaEnvironmentDeployment.id,
          schemaDestructiveVolumeAttempt.environmentDeploymentId,
        ),
      )
      .innerJoin(
        schemaEnvironment,
        eq(schemaEnvironment.id, schemaEnvironmentDeployment.environmentId),
      )
      .where(
        and(
          inArray(schemaDestructiveVolumeAttempt.disposition, ["active", "accepted"]),
          isNotNull(schemaDestructiveVolumeAttempt.inngestRunId),
          isNotNull(schemaDestructiveVolumeAttempt.deadlineAt),
          lte(schemaDestructiveVolumeAttempt.deadlineAt, input.deadlineAtOrBefore),
          input.after
            ? or(
                gt(schemaDestructiveVolumeAttempt.deadlineAt, input.after.deadlineAt),
                and(
                  eq(schemaDestructiveVolumeAttempt.deadlineAt, input.after.deadlineAt),
                  gt(schemaDestructiveVolumeAttempt.id, input.after.id),
                ),
              )
            : undefined,
        ),
      )
      .orderBy(
        asc(schemaDestructiveVolumeAttempt.deadlineAt),
        asc(schemaDestructiveVolumeAttempt.id),
      )
      .limit(limit);
    return rows.map((row) => ({
      ...row,
      // SAFETY: the query requires isNotNull(deadlineAt); Drizzle still types the column as Date | null.
      deadlineAt: row.deadlineAt as Date,
    }));
  }).pipe(
    Effect.mapError(
      destructiveVolumeRepositoryError,
    ),
  );
}

export function listDestructiveVolumeAttemptsForOrganization(organizationId: string) {
  return Effect.gen(function* () {
    const database = yield* Database;
    const rows = yield* database.drizzle
      .select({ attempt: schemaDestructiveVolumeAttempt })
      .from(schemaDestructiveVolumeAttempt)
      .innerJoin(
        schemaEnvironmentDeployment,
        eq(
          schemaEnvironmentDeployment.id,
          schemaDestructiveVolumeAttempt.environmentDeploymentId,
        ),
      )
      .innerJoin(
        schemaEnvironment,
        eq(schemaEnvironment.id, schemaEnvironmentDeployment.environmentId),
      )
      .where(eq(schemaEnvironment.organizationId, organizationId))
      .orderBy(desc(schemaDestructiveVolumeAttempt.createdAt));
    return rows.map(({ attempt }) => attempt);
  }).pipe(
    Effect.mapError(destructiveVolumeRepositoryError),
  );
}
