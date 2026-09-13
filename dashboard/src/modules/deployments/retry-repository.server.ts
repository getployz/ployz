import "@tanstack/react-start/server-only";
import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import {
  volumeRemoveAttempt as schemaVolumeRemoveAttempt,
} from "#/modules/runtime/tables";
import {
  environment as schemaEnvironment,
  project as schemaProject,
} from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { NotFound, Validation } from "#/server/public-error";
import { withMutationResult } from "#/server/mutation-result.server";
import {
  listCoreOperationEvidencePageEffect,
} from "#/modules/operations/core-operation-evidence.server";
import {
  admitActiveDeploymentAttempt,
  isActiveDeploymentUniqueViolation,
} from "#/modules/deployments/queue-lock.server";
import { admitEnvironmentDeployment } from "#/modules/deployments/admission.server";
import { dispatchEnvironmentDeployment } from "#/modules/deployments/dispatch.server";

const activeAttemptConflict = () =>
  new Validation({
    field: "environmentId",
    message: "An environment deployment attempt is already active.",
  });

export const loadAuthorizedDeploymentEvidence = Effect.fn(
  "Deployments.loadAuthorizedDeploymentEvidence",
)(function* (input: {
  readonly organizationId: string;
  readonly deploymentId: string;
  readonly afterSequence?: string;
  readonly limit: number;
}) {
  const { drizzle: database } = yield* Database;
  const selected = yield* database
    .select({ coreDeployId: schemaEnvironmentDeployment.coreDeployId })
    .from(schemaEnvironmentDeployment)
    .innerJoin(
      schemaEnvironment,
      eq(schemaEnvironment.id, schemaEnvironmentDeployment.environmentId),
    )
    .innerJoin(
      schemaProject,
      eq(schemaProject.id, schemaEnvironment.projectId),
    )
    .where(
      and(
        eq(schemaEnvironmentDeployment.id, input.deploymentId),
        eq(schemaProject.organizationId, input.organizationId),
      ),
    )
    .limit(1);
  const [deployment] = selected;
  if (!deployment) {
    return yield* new NotFound({
      message: "The environment deployment was not found.",
    });
  }
  if (!deployment.coreDeployId) return null;
  const coreOperationId = deployment.coreDeployId;
  return yield* listCoreOperationEvidencePageEffect({
    organizationId: input.organizationId,
    coreOperationId,
    afterSequence: input.afterSequence,
    limit: input.limit,
  }, database);
});

export const createRetryAttempt = Effect.fn("Deployments.createRetryAttempt")(
  function* (input: {
    readonly environmentId: string;
    readonly userId: string;
    readonly failedDeploymentId: string;
  }) {
    const database = yield* Database;
    const selected = yield* database.drizzle
      .select({
        id: schemaEnvironmentDeployment.id,
        status: schemaEnvironmentDeployment.status,
        message: schemaEnvironmentDeployment.message,
        savedStateSnapshotId:
          schemaEnvironmentDeployment.savedStateSnapshotId,
      })
      .from(schemaEnvironmentDeployment)
      .where(
        and(
          eq(schemaEnvironmentDeployment.id, input.failedDeploymentId),
          eq(schemaEnvironmentDeployment.environmentId, input.environmentId),
        ),
      )
      .limit(1);
    const [source] = selected;
    if (!source) {
      return yield* new NotFound({
        message: "The environment deployment was not found.",
      });
    }
    if (source.status !== "failed") {
      return yield* new Validation({
        field: "failedDeploymentId",
        message: "Only failed deployment attempts can be retried.",
      });
    }
    const created = yield* withMutationResult(
      Effect.gen(function* () {
        const { drizzle: tx } = yield* Database;
        yield* admitActiveDeploymentAttempt(input.environmentId);
        const [lockedSource] = yield* tx
          .select({
            status: schemaEnvironmentDeployment.status,
          })
          .from(schemaEnvironmentDeployment)
          .where(eq(schemaEnvironmentDeployment.id, source.id))
          .for("update");
        if (lockedSource?.status !== "failed") {
          return yield* new Validation({
            field: "failedDeploymentId",
            message: "The failed attempt is no longer retryable.",
          });
        }
        const [volumeRemoveAttempt] = yield* tx
          .select({ id: schemaVolumeRemoveAttempt.id })
          .from(schemaVolumeRemoveAttempt)
          .where(
            eq(
              schemaVolumeRemoveAttempt.environmentDeploymentId,
              source.id,
            ),
          )
          .limit(1);
        if (volumeRemoveAttempt) {
          return yield* new Validation({
            field: "failedDeploymentId",
            message:
              "Volume removal retries require a fresh destructive review from the environment canvas.",
          });
        }
        const deployment = yield* admitEnvironmentDeployment({
          environmentId: input.environmentId,
          triggerOrigin: { origin: "manual", actorId: input.userId },
          message: source.message,
          retryOfDeploymentId: source.id,
          savedStateSnapshotId: source.savedStateSnapshotId,
        });
        return {
          environmentDeploymentId: deployment.id,
          status: deployment.status,
          createdAt: deployment.createdAt,
          serviceCount: deployment.serviceCount,
          retryOfDeploymentId: source.id,
        };
      }),
    ).pipe(
      Effect.catchIf(isActiveDeploymentUniqueViolation, activeAttemptConflict),
    );
    yield* dispatchEnvironmentDeployment({
      environmentDeploymentId: created.data.environmentDeploymentId,
      environmentId: input.environmentId,
    });
    return created;
  },
);
