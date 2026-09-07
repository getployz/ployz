import "@tanstack/react-start/server-only";
import { and, eq, inArray, sql } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { Database, sqlErrorFrom } from "#/server/database.server";
import { Validation } from "#/server/public-error";
import { ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES } from "#/modules/deployments/runtime-contract";

const ACTIVE_DEPLOYMENT_CONSTRAINTS = new Set([
  "environment_deployment_one_queued_target_idx",
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
