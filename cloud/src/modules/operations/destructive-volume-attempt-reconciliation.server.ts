import { volumeIsAuthored } from "#/modules/environment-design/document-identity.server";
import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import {
  environmentResource as schemaEnvironmentResource,
} from "#/modules/environment-design/tables";
import {
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
} from "#/modules/operations/tables";
import { getVolumePhysicalName } from "#/modules/environment-design/volume-config";
import {
  loadDestructiveVolumeAttempt,
} from "#/modules/operations/destructive-volume-attempt-persistence.server";
import {
  recordDestructiveVolumeEvent,
} from "#/modules/operations/destructive-volume-attempt-workflow.server";
import {
  DestructiveVolumeConflict,
  DestructiveVolumePersistenceFailure,
} from "#/modules/operations/destructive-volume-errors";
import { Database } from "#/server/database.server";

export const completeDestructiveVolumeAttempt = Effect.fn(
  "Operations.completeDestructiveVolumeAttempt",
)(function* (input: {
  attemptId: string;
  operationId: string;
  now?: Date;
}) {
  const database = yield* Database;
  return yield* database
    .transaction(
      Effect.gen(function* () {
        const tx = (yield* Database).drizzle;
        const attempt = yield* loadDestructiveVolumeAttempt(input.attemptId);
        if (
          !attempt ||
          attempt.disposition !== "accepted" ||
          attempt.operationId !== input.operationId
        ) {
          return yield* new DestructiveVolumeConflict({
            message: "Destructive volume completion authority conflicts.",
          });
        }
        const [authority] = yield* tx
          .select({
            resourceId: schemaEnvironmentResource.id,
            environmentId: schemaEnvironmentResource.environmentId,
            implementationType: schemaEnvironmentResource.implementationType,
            isAuthored: volumeIsAuthored,
            deploymentEnvironmentId: schemaEnvironmentDeployment.environmentId,
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
            schemaEnvironmentResource,
            and(
              eq(
                schemaEnvironmentResource.id,
                schemaDestructiveVolumeAttempt.environmentResourceId,
              ),
              eq(
                schemaEnvironmentResource.environmentId,
                schemaEnvironmentDeployment.environmentId,
              ),
            ),
          )
          .where(eq(schemaDestructiveVolumeAttempt.id, attempt.id))
          .limit(1);
        const hasExactAuthority =
          authority?.implementationType === "volume" &&
          !authority.isAuthored &&
          authority.environmentId === authority.deploymentEnvironmentId &&
          getVolumePhysicalName(authority.resourceId) === attempt.target.volumeName;
        const reconciled = hasExactAuthority;
        return yield* recordDestructiveVolumeEvent({
          attemptId: input.attemptId,
          event: reconciled
            ? { event: "core_completed", operationId: input.operationId }
            : {
                event: "reconciliation_failed",
                operationId: input.operationId,
                code: "tombstone_not_reconciled",
                message:
                  "Core removed the volume, but Dashboard could not reconcile the exact tombstoned resource.",
              },
          now: input.now ?? new Date(),
        });
      }),
    )
    .pipe(
      Effect.mapError((cause) =>
        cause instanceof DestructiveVolumeConflict ||
        cause instanceof DestructiveVolumePersistenceFailure
          ? cause
          : new DestructiveVolumePersistenceFailure({ cause }),
      ),
    );
});
