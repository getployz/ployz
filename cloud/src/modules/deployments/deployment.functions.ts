import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import {
  confirmEnvironmentDeploymentSchema,
  createEnvironmentDeploymentSnapshotSchema,
  deploymentOperationEvidencePageQuerySchema,
  discardEnvironmentSavedChangeSchema,
  dispatchQueuedEnvironmentDeploymentSchema,
  organizationEnvironmentChangeStateQuerySchema,
  prepareDestructiveVolumeRetrySchema,
  prepareEnvironmentDestructiveVolumesSchema,
  retryDestructiveVolumeAttemptSchema,
  retryEnvironmentDeploymentSchema,
} from "#/modules/deployments/deployment-contract";
import {
  confirmEnvironmentDeployment,
  createEnvironmentDeploymentSnapshot,
  discardEnvironmentSavedChange,
  dispatchExistingQueuedEnvironmentDeployment,
  listDeploymentOperationEvidence,
  listLatestOrganizationEnvironmentChangeStates,
  prepareDestructiveVolumeRetry,
  prepareEnvironmentDestructiveVolumes,
  retryDestructiveVolumeAttempt,
  retryEnvironmentDeployment,
} from "#/modules/deployments/deployment-operations.server";
import {
  actorMiddleware,
  publicErrorMiddleware,
  runActor,
  strictValidator,
} from "#/server/tanstack";

const deploymentMiddleware = [publicErrorMiddleware, actorMiddleware] as const;

export const listLatestOrganizationEnvironmentChangeStatesServerFn =
  createServerFn({ method: "GET" })
    .middleware(deploymentMiddleware)
    .validator(strictValidator(organizationEnvironmentChangeStateQuerySchema))
    .handler(({ context, data }) =>
      runActor(
        context,
        listLatestOrganizationEnvironmentChangeStates(context.actor, data),
      ),
    );

export const createEnvironmentDeploymentSnapshotServerFn = createServerFn({
  method: "POST",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(createEnvironmentDeploymentSnapshotSchema))
  .handler(({ context, data }) =>
    runActor(
      context,
      createEnvironmentDeploymentSnapshot(context.actor, data).pipe(
        Effect.catchTag("DestructiveVolumeReviewChangedError", (cause) =>
          Effect.succeed({
            state: "review_updated_evidence" as const,
            freshReviews: cause.freshReviews,
          }),
        ),
      ),
    ),
  );

export const discardEnvironmentSavedChangeServerFn = createServerFn({
  method: "POST",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(discardEnvironmentSavedChangeSchema))
  .handler(({ context, data }) =>
    runActor(context, discardEnvironmentSavedChange(context.actor, data)),
  );

export const prepareEnvironmentDestructiveVolumesServerFn = createServerFn({
  method: "POST",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(prepareEnvironmentDestructiveVolumesSchema))
  .handler(({ context, data }) =>
    runActor(context, prepareEnvironmentDestructiveVolumes(context.actor, data)),
  );

export const prepareDestructiveVolumeRetryServerFn = createServerFn({
  method: "POST",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(prepareDestructiveVolumeRetrySchema))
  .handler(({ context, data }) =>
    runActor(context, prepareDestructiveVolumeRetry(context.actor, data)),
  );

export const retryDestructiveVolumeAttemptServerFn = createServerFn({
  method: "POST",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(retryDestructiveVolumeAttemptSchema))
  .handler(({ context, data }) =>
    runActor(
      context,
      retryDestructiveVolumeAttempt(context.actor, data).pipe(
        Effect.catchTag("DestructiveVolumeReviewChangedError", (cause) =>
          Effect.succeed({
            state: "review_updated_evidence" as const,
            freshReviews: cause.freshReviews,
          }),
        ),
      ),
    ),
  );

export const retryEnvironmentDeploymentServerFn = createServerFn({
  method: "POST",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(retryEnvironmentDeploymentSchema))
  .handler(({ context, data }) =>
    runActor(context, retryEnvironmentDeployment(context.actor, data)),
  );

export const confirmEnvironmentDeploymentServerFn = createServerFn({
  method: "POST",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(confirmEnvironmentDeploymentSchema))
  .handler(({ context, data }) =>
    runActor(context, confirmEnvironmentDeployment(context.actor, data)),
  );

export const dispatchQueuedEnvironmentDeploymentServerFn = createServerFn({
  method: "POST",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(dispatchQueuedEnvironmentDeploymentSchema))
  .handler(({ context, data }) =>
    runActor(
      context,
      dispatchExistingQueuedEnvironmentDeployment(context.actor, data),
    ),
  );

export const listDeploymentOperationEvidenceServerFn = createServerFn({
  method: "GET",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(deploymentOperationEvidencePageQuerySchema))
  .handler(({ context, data }) =>
    runActor(context, listDeploymentOperationEvidence(context.actor, data)),
  );
