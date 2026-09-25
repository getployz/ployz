import "@tanstack/react-start/server-only";
import { and, eq, inArray, isNotNull, isNull, sql } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { Database, sqlErrorFrom } from "#/server/database.server";
import { Validation } from "#/server/public-error";
import { ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES } from "#/modules/deployments/runtime-contract";

/** The Environment's building attempt: queued, and its run has started its Image Builds. */
export const buildingAttemptOf = (environmentId: string) => and(
  eq(schemaEnvironmentDeployment.environmentId, environmentId),
  eq(schemaEnvironmentDeployment.status, "queued"),
  isNotNull(schemaEnvironmentDeployment.inngestRunId),
);

/** The Environment's pending attempt: queued with no run yet; the newest admission replaces it. */
export const pendingAttemptOf = (environmentId: string) => and(
  eq(schemaEnvironmentDeployment.environmentId, environmentId),
  eq(schemaEnvironmentDeployment.status, "queued"),
  isNull(schemaEnvironmentDeployment.inngestRunId),
);

const ACTIVE_DEPLOYMENT_CONSTRAINTS = new Set([
  "environment_deployment_one_building_attempt_idx",
  "environment_deployment_one_pending_attempt_idx",
  "environment_deployment_one_started_attempt_idx",
  "environment_deployment_one_active_attempt_idx",
]);

export function isActiveDeploymentUniqueViolation(cause: unknown) {
  const sqlError = sqlErrorFrom(cause);
  return (
    sqlError !== undefined &&
    sqlError.reason._tag === "UniqueViolation" &&
    ACTIVE_DEPLOYMENT_CONSTRAINTS.has(sqlError.reason.constraint)
  );
}

export const admitActiveDeploymentAttempt = Effect.fn(
  "Deployments.admitActiveDeploymentAttempt",
)(function* (environmentId: string) {
  yield* lockEnvironmentDeploymentQueue(environmentId);
  const { drizzle } = yield* Database;
  const [active] = yield* drizzle
    .select({ id: schemaEnvironmentDeployment.id })
    .from(schemaEnvironmentDeployment)
    .where(
      and(
        eq(schemaEnvironmentDeployment.environmentId, environmentId),
        inArray(schemaEnvironmentDeployment.status, [
          ...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES,
        ]),
      ),
    )
    .limit(1);

  if (active) {
    return yield* new Validation({
      field: "environmentId",
      message: "An environment deployment attempt is already active.",
    });
  }
});

export const lockEnvironmentDeploymentQueue = Effect.fn(
  "Deployments.lockEnvironmentDeploymentQueue",
)(function* (environmentId: string) {
  const { drizzle } = yield* Database;
  yield* drizzle.execute(
    sql`select pg_advisory_xact_lock(hashtext(${environmentId}))`,
  );
});
