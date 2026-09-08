import { createServerFn } from "@tanstack/react-start";
import { Effect } from "effect";
import {
  createEnvironmentDeploymentSnapshotSchema,
  deploymentOperationEvidencePageQuerySchema,
  discardEnvironmentSavedChangeSchema,
  dispatchQueuedEnvironmentDeploymentSchema,
  organizationEnvironmentChangeStateQuerySchema,
  prepareEnvironmentDestructiveVolumesSchema,
  retryEnvironmentDeploymentSchema,
} from "#/modules/deployments/deployment-contract";
import {
  createEnvironmentDeploymentSnapshot,
  discardEnvironmentSavedChange,
  dispatchExistingQueuedEnvironmentDeployment,
  listDeploymentOperationEvidence,
  listLatestOrganizationEnvironmentChangeStates,
  prepareEnvironmentDestructiveVolumes,
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

export const retryEnvironmentDeploymentServerFn = createServerFn({
  method: "POST",
})
  .middleware(deploymentMiddleware)
  .validator(strictValidator(retryEnvironmentDeploymentSchema))
  .handler(({ context, data }) =>
    runActor(context, retryEnvironmentDeployment(context.actor, data)),
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
