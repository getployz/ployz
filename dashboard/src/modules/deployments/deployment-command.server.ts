import { requestDeploymentCancellation } from "./runtime-cancellation.repository.server";
import { ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES } from "./runtime-contract";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createEnvironmentDeployCancelRequestedEvent } from "#/modules/inngest/events";
import type { CancelEnvironmentDeploymentInput } from "./deployment-contract";
import "@tanstack/react-start/server-only";

import { and, eq } from "drizzle-orm";
import { Effect } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { afterDatabaseCommit, Database } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { withMutationResult } from "#/server/mutation-result.server";
import { isActiveDeploymentUniqueViolation } from "#/modules/deployments/queue-lock.server";
import { createRetryAttempt } from "#/modules/deployments/retry-repository.server";
import { loadCurrentEnvironmentSnapshotProjection } from "#/modules/environment-design/working-state-repository.server";
import { gatherExactTombstonedVolumeReviews } from "#/modules/deployments/destructive-volume-review.server";
import { loadEnvironmentSnapshotProjection } from "#/modules/deployments/environment-state.repository.server";
import type { Actor } from "#/modules/identity/actor";

import { getEnvironmentContextForActor } from "#/modules/environment-design/authoring-repository.server";

import { DestructiveVolumeReviewChangedError, getDestructiveVolumeReviewMismatch } from "#/modules/environment-design/destructive-volume-review";
import { saveReviewedEnvironmentState } from "#/modules/environment-design/saved-state-operations.server";
import { getDestructiveEnvironmentSaveReviewMismatch, projectDestructiveEnvironmentSave } from "#/modules/environment-design/working-state-review";
import { admitEnvironmentDeployment } from "./admission.server";
import { lockEnvironmentDeploymentQueue } from "./queue-lock.server";
import type { ReviewedEnvironmentPublication } from "#/modules/environment-design/working-state-review";
import { dispatchEnvironmentDeployment } from "#/modules/deployments/dispatch.server";
import type { ReviewedPublicationInput, DispatchQueuedEnvironmentDeploymentInput, RetryEnvironmentDeploymentInput } from "#/modules/deployments/deployment-contract";

type EnvironmentContextInput = {
  readonly organizationSlug: string;
  readonly projectSlug: string;
  readonly environmentSlug: string;
};

const requireEnvironment = Effect.fn("Deployments.requireEnvironment")(
  function* (actor: Actor, input: EnvironmentContextInput) {
    const context = yield* getEnvironmentContextForActor(actor, input);
    if (context !== null) return context;
    return yield* new NotFound({
      message: "The environment was not found.",
    });
  },
);

const loadDestructiveEnvironmentSave = Effect.fn(
  "Deployments.loadDestructiveEnvironmentSave",
)(function* (environmentId: string) {
  const database = yield* Database;
  const snapshotProjection = yield* loadEnvironmentSnapshotProjection({
    kind: "environment",
    environmentId,
  });
  const workingProjection = yield* database.transaction(
    loadCurrentEnvironmentSnapshotProjection(environmentId),
  );
  const explicitState = snapshotProjection.explicitStates.find(
    (state) => state.environmentId === environmentId,
  );
  return projectDestructiveEnvironmentSave({
    workingNodes: workingProjection.nodeSnapshots,
    appliedNodes: explicitState?.applied.nodes ?? [],
  });
});

export const prepareEnvironmentDestructiveVolumes = Effect.fn(
  "Deployments.prepareEnvironmentDestructiveVolumes",
)(function* (actor: Actor, input: EnvironmentContextInput) {
  const context = yield* requireEnvironment(actor, input);
  const destructiveSave = yield* loadDestructiveEnvironmentSave(
    context.environment.id,
  );
  return yield* gatherExactTombstonedVolumeReviews({
      actor,
      organizationSlug: input.organizationSlug,
      environmentId: context.environment.id,
      resourceIds: destructiveSave.volumeIds,
    });
});

export const submitReviewedPublication = Effect.fn(
  "Deployments.submitReviewedPublication",
)(function* (actor: Actor, input: ReviewedPublicationInput) {
  const context = yield* requireEnvironment(actor, input);
  const shouldDeploy = input.intent === "manual_deploy";
  const message = input.message?.trim() || null;
  const destructiveVolumeReviews = input.review.destructiveVolumeReviews;
  const reviewedDestructiveSave = {
    serviceIds: input.review.destructiveServiceIds,
    volumeIds: destructiveVolumeReviews.map((review) => review.target.resourceId),
  };
  const destructiveSave = yield* loadDestructiveEnvironmentSave(
    context.environment.id,
  );
  const destructiveReviewMismatch = getDestructiveEnvironmentSaveReviewMismatch({
    expected: destructiveSave,
    reviewed: reviewedDestructiveSave,
  });
  if (destructiveReviewMismatch !== null) {
    return yield* new Conflict({
      message: destructiveReviewMismatch,
    });
  }
  if (destructiveSave.volumeIds.length > 0) {
    const freshReviews = yield* gatherExactTombstonedVolumeReviews({
        actor,
        organizationSlug: input.organizationSlug,
        environmentId: context.environment.id,
        resourceIds: destructiveSave.volumeIds,
      });
    const mismatch = getDestructiveVolumeReviewMismatch({
      reviewed: destructiveVolumeReviews,
      fresh: freshReviews,
    });
    if (mismatch !== null) {
      return yield* new DestructiveVolumeReviewChangedError({
        reason: "review_updated_evidence",
        message: mismatch,
        freshReviews,
      });
    }
  }

  const publication = {
    environmentId: context.environment.id,
    actorId: actor.userId,
    message,
    review: input.review,
  };
  if (shouldDeploy) {
    const attempt = yield* withMutationResult(
      createManualEnvironmentDeployment(publication),
      { isolationLevel: "read committed" },
    ).pipe(
      Effect.catchIf(isActiveDeploymentUniqueViolation, () =>
        new Validation({
          field: "environmentId",
          message: "An environment deployment attempt is already active.",
        }),
      ),
    );
    return yield* dispatchEnvironmentDeployment({
      environmentDeploymentId: attempt.data.environmentDeploymentId,
      environmentId: context.environment.id,
    }).pipe(
      Effect.as({ state: "deployment_queued" as const, deploymentId: attempt.data.environmentDeploymentId }),
      Effect.catchTag("InngestEventSendError", () =>
        Effect.succeed({ state: "attempt_dispatch_failed" as const }),
      ),
    );
  }

  yield* withMutationResult(
    saveReviewedEnvironmentState(publication),
    { isolationLevel: "read committed" },
  );
  return { state: "saved" as const };
});

export const dispatchExistingQueuedEnvironmentDeployment = Effect.fn(
  "Deployments.dispatchExistingQueuedEnvironmentDeployment",
)(function* (actor: Actor, input: DispatchQueuedEnvironmentDeploymentInput) {
  const { drizzle: database } = yield* Database;
  const context = yield* requireEnvironment(actor, input);
  const rows = yield* database
        .select({ id: schemaEnvironmentDeployment.id })
        .from(schemaEnvironmentDeployment)
        .where(
          and(
            eq(schemaEnvironmentDeployment.environmentId, context.environment.id),
            eq(schemaEnvironmentDeployment.status, "queued"),
          ),
        )
        .limit(1);
  const queued = rows[0];
  if (queued === undefined) {
    return yield* new NotFound({
      message: "A queued environment deployment was not found.",
    });
  }
  return yield* dispatchEnvironmentDeployment({
    environmentDeploymentId: queued.id,
    environmentId: context.environment.id,
  });
});

export const retryEnvironmentDeployment = Effect.fn(
  "Deployments.retryEnvironmentDeployment",
)(function* (actor: Actor, input: RetryEnvironmentDeploymentInput) {
  const context = yield* requireEnvironment(actor, input);
  return yield* createRetryAttempt({
      environmentId: context.environment.id,
      userId: actor.userId,
      failedDeploymentId: input.failedDeploymentId,
    });
});

export const cancelEnvironmentDeployment = Effect.fn("Deployments.cancelEnvironmentDeployment")(
  function* (actor: Actor, input: CancelEnvironmentDeploymentInput) {
    const context = yield* requireEnvironment(actor, input);
    const { drizzle } = yield* Database;
    const [deployment] = yield* drizzle.select().from(schemaEnvironmentDeployment).where(and(
      eq(schemaEnvironmentDeployment.id, input.deploymentId),
      eq(schemaEnvironmentDeployment.environmentId, context.environment.id),
    )).limit(1);
    if (!deployment) return yield* new NotFound({ message: "Deployment not found." });
    if (!ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES.has(deployment.status)) return;
    const cancelled = yield* requestDeploymentCancellation(deployment.id);
    if (cancelled) yield* afterDatabaseCommit(sendInngestEvent(createEnvironmentDeployCancelRequestedEvent(deployment.id)));
  },
);

export const createManualEnvironmentDeployment = Effect.fn(
  "Deployments.createManualEnvironmentDeployment",
)(function* (input: {
  readonly environmentId: string;
  readonly actorId: string;
  readonly message: string | null;
  readonly review: ReviewedEnvironmentPublication;
}) {
  const database = yield* Database;
  return yield* database.transaction(Effect.gen(function* () {
    yield* lockEnvironmentDeploymentQueue(input.environmentId);
    const saved = yield* saveReviewedEnvironmentState({
      ...input,
      revisionPolicy: "reuse_latest_if_equivalent",
    });

    const deployment = yield* admitEnvironmentDeployment({
      environmentId: input.environmentId,
      triggerOrigin: { origin: "manual", actorId: input.actorId },
      message: input.message,
      savedStateSnapshotId: saved.savedStateSnapshotId,
      serviceActionPolicy: { kind: "all_affected_required" },
      freshVolumeReviewIds: input.review.destructiveVolumeReviews.map(review => review.target.resourceId),
    });

    return {
      environmentDeploymentId: deployment.id,
      status: deployment.status,
      createdAt: deployment.createdAt,
      serviceCount: saved.nodeSnapshots.filter(
        ({ nodeType }) => nodeType === "service",
      ).length,
    };
  }));
});
