import "@tanstack/react-start/server-only";
import { and, eq, inArray } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { coreOperationWatch as schemaCoreOperationWatch } from "#/modules/operations/tables";
import { organization as schemaOrganization } from "#/modules/organization/tables";
import {
  environment as schemaEnvironment,
  project as schemaProject,
} from "#/modules/project/tables";
import { ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES } from "#/modules/deployments/runtime-contract";
import {
  failAwaitingVolumeRemoveAttemptsForDeploymentInTransaction,
} from "#/modules/runtime/volume-removal.repository";
import { Database } from "#/server/database.server";

export const markCancelledByInngestRunId = Effect.fn(
  "Deployments.markCancelledByInngestRunId",
)(function* (runId: string, message = "Cancelled in Inngest.") {
  const database = yield* Database;
  const [record] = yield* database.drizzle
    .select({
      deployment: schemaEnvironmentDeployment,
      organizationId: schemaOrganization.id,
    })
    .from(schemaEnvironmentDeployment)
    .innerJoin(
      schemaEnvironment,
      eq(schemaEnvironment.id, schemaEnvironmentDeployment.environmentId),
    )
    .innerJoin(schemaProject, eq(schemaProject.id, schemaEnvironment.projectId))
    .innerJoin(
      schemaOrganization,
      eq(schemaOrganization.id, schemaProject.organizationId),
    )
    .where(eq(schemaEnvironmentDeployment.inngestRunId, runId))
    .limit(1);
  if (!record || record.deployment.status === "cancelled") return false;
  if (!ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES.has(record.deployment.status)) {
    return false;
  }
  const updatedAt = new Date();
  return yield* database.transaction(
    Effect.gen(function* () {
      const tx = (yield* Database).drizzle;
      const updated = yield* tx
        .update(schemaEnvironmentDeployment)
        .set({
          status: "cancelled",
          cancellationRequestedAt: updatedAt,
          failureMessage: message,
          finishedAt: updatedAt,
          updatedAt,
        })
        .where(
          and(
            eq(schemaEnvironmentDeployment.id, record.deployment.id),
            eq(schemaEnvironmentDeployment.inngestRunId, runId),
            inArray(schemaEnvironmentDeployment.status, [
              ...ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES,
            ]),
          ),
        )
        .returning({
          id: schemaEnvironmentDeployment.id,
          coreDeployId: schemaEnvironmentDeployment.coreDeployId,
        });
      if (updated.length === 0) return false;
      const coreDeployId = updated[0]?.coreDeployId;
      if (coreDeployId) {
        yield* tx
          .update(schemaCoreOperationWatch)
          .set({
            observationState: "cloud_cancelled",
            terminalAt: updatedAt,
            updatedAt,
          })
          .where(
            and(
              eq(schemaCoreOperationWatch.organizationId, record.organizationId),
              eq(schemaCoreOperationWatch.operationId, coreDeployId),
              eq(schemaCoreOperationWatch.observationState, "active"),
            ),
          );
      }
      yield* failAwaitingVolumeRemoveAttemptsForDeploymentInTransaction(
        tx,
        {
          environmentDeploymentId: record.deployment.id,
          deploymentDisposition: "cancelled",
          now: updatedAt,
        },
      );
      return true;
    }),
  );
});
