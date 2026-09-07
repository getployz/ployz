import "@tanstack/react-start/server-only";

import { and, eq, isNull } from "drizzle-orm";
import { Effect, Schema } from "effect";
import { environmentDeployment as schemaEnvironmentDeployment } from "#/modules/deployments/tables";
import { service as schemaService } from "#/modules/environment-design/tables";
import {
  destructiveVolumeAttempt as schemaDestructiveVolumeAttempt,
} from "#/modules/operations/tables";
import { environment as schemaEnvironment } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { DeployImageNotPullableError } from "#/modules/deployments/deployment-errors";
import { isActiveDeploymentUniqueViolation } from "#/modules/deployments/queue-lock.server";
import {
  destructiveVolumeRetryModeForAttempt,
} from "#/modules/operations/destructive-volume-attempt";
import {
  retryDestructiveVolumeAttempt as retryDestructiveVolumeAttemptRecord,
} from "#/modules/operations/destructive-volume-attempt.repository";
import {
  completeDestructiveVolumeAttempt,
} from "#/modules/operations/destructive-volume-attempt-reconciliation.server";
import {
  dispatchDestructiveVolumeAttempt,
} from "#/modules/operations/destructive-volume-dispatch.server";
import {
  createRetryAttempt,
  loadAuthorizedDeploymentEvidence,
} from "#/modules/deployments/retry-repository.server";
import {
  loadCurrentEnvironmentSnapshotProjection,
} from "#/modules/environment-design/working-state-repository.server";
import {
  gatherExactTombstonedVolumeReviews,
} from "#/modules/deployments/destructive-volume-review.server";
import {
  loadEnvironmentSnapshotProjection,
  type EnvironmentSnapshotProjection,
} from "#/modules/deployments/environment-state.repository.server";
import { findUnpullableSdkDeployImages } from "#/modules/deployments/image-gate";
import { sendInngestEvent } from "#/modules/inngest/client";
import { createEnvironmentDeployConfirmedEvent } from "#/modules/inngest/events";
import type { Actor } from "#/modules/identity/actor";
import { serviceDeploymentConfigSchema } from "#/modules/environment-design/services";
import { decodeEnvironmentResourceNodeConfig } from "#/modules/environment-design/environment-resource-node";
import { strictParseOptions } from "#/modules/environment-design/schema";
import {
  getEnvironmentContextForActor,
} from "#/modules/environment-design/authoring-repository.server";
import {
  getOrganizationForUserBySlug,
} from "#/modules/environment-design/workspace-repository.server";
import {
  DestructiveVolumeReviewChangedError,
  getDestructiveVolumeReviewMismatch,
} from "#/modules/environment-design/destructive-volume-review";
import {
  discardEnvironmentSavedState,
  saveReviewedEnvironmentState,
} from "#/modules/environment-design/saved-state-operations.server";
import {
  getDestructiveEnvironmentSaveReviewMismatch,
  projectDestructiveEnvironmentSave,
} from "#/modules/environment-design/working-state-review";
import {
  createManualEnvironmentDeployment,
} from "#/modules/deployments/manual-admission.server";
import { dispatchEnvironmentDeployment } from "#/modules/deployments/dispatch.server";
import { requireConfirmableSdkDeployPreview } from "#/modules/deployments/runtime-preview";
import type {
  ConfirmEnvironmentDeploymentInput,
  CreateEnvironmentDeploymentSnapshotInput,
  DeploymentOperationEvidencePageQueryInput,
  DiscardEnvironmentSavedChangeInput,
  DispatchQueuedEnvironmentDeploymentInput,
  EnvironmentChangeStateNodeProjection,
  EnvironmentChangeStateProjection,
  OrganizationEnvironmentChangeStateQueryInput,
  RetryEnvironmentDeploymentInput,
  DestructiveVolumeReview,
} from "#/modules/deployments/deployment-contract";

type EnvironmentContextInput = {
  readonly organizationSlug: string;
  readonly projectSlug: string;
  readonly environmentSlug: string;
};

function requirePullableSdkDeployImagesEffect(
  input: Parameters<typeof findUnpullableSdkDeployImages>[0],
): Effect.Effect<void, DeployImageNotPullableError> {
  const error = findUnpullableSdkDeployImages(input);
  return error ? Effect.fail(error) : Effect.void;
}

const requireEnvironment = Effect.fn("Deployments.requireEnvironment")(
  function* (actor: Actor, input: EnvironmentContextInput) {
    const context = yield* getEnvironmentContextForActor(actor, input);
    if (context !== null) return context;
    return yield* new NotFound({
      message: "The environment was not found.",
    });
  },
);

const requireOrganization = Effect.fn("Deployments.requireOrganization")(
  function* (actor: Actor, organizationSlug: string) {
    const organization = yield* getOrganizationForUserBySlug(
      actor.userId,
      organizationSlug,
    );
    if (organization !== null) return organization;
    return yield* new NotFound({
      message: "The organization was not found.",
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
    savedNodes: explicitState?.saved?.nodes ?? [],
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

const loadAuthorizedDestructiveVolumeRetry = Effect.fn(
  "Deployments.loadAuthorizedDestructiveVolumeRetry",
)(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly attemptId: string },
) {
  const { drizzle: database } = yield* Database;
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const rows = yield* database
        .select({
          attempt: schemaDestructiveVolumeAttempt,
          environmentId: schemaEnvironment.id,
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
          eq(
            schemaEnvironment.id,
            schemaEnvironmentDeployment.environmentId,
          ),
        )
        .where(
          and(
            eq(schemaDestructiveVolumeAttempt.id, input.attemptId),
            eq(schemaEnvironment.organizationId, organization.id),
          ),
        )
        .limit(1);
  const source = rows[0];
  if (source === undefined) {
    return yield* new NotFound({
      message: "The destructive volume attempt was not found.",
    });
  }
  const mode = destructiveVolumeRetryModeForAttempt(source.attempt);
  if (mode === null) {
    return yield* new Conflict({
      message: "This volume removal attempt has no safe retry action.",
    });
  }
  return { source, mode };
});

export const prepareDestructiveVolumeRetry = Effect.fn(
  "Deployments.prepareDestructiveVolumeRetry",
)(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly attemptId: string },
) {
  const { source, mode } = yield* loadAuthorizedDestructiveVolumeRetry(
    actor,
    input,
  );
  if (mode === "new_removal") {
    const reviews = yield* gatherExactTombstonedVolumeReviews({
      actor,
      organizationSlug: input.organizationSlug,
      environmentId: source.environmentId,
      resourceIds: [source.attempt.environmentResourceId],
    });
    return { mode, reviews };
  }
  return {
    mode,
    reviews: [
      { target: source.attempt.target, evidence: source.attempt.evidence },
    ],
  };
});

export const retryDestructiveVolumeAttempt = Effect.fn(
  "Deployments.retryDestructiveVolumeAttempt",
)(function* (
  actor: Actor,
  input: {
    readonly organizationSlug: string;
    readonly attemptId: string;
    readonly review: DestructiveVolumeReview;
  },
) {
  const { source, mode } = yield* loadAuthorizedDestructiveVolumeRetry(
    actor,
    input,
  );
  if (mode === "new_removal") {
    const freshReviews = yield* gatherExactTombstonedVolumeReviews({
      actor,
      organizationSlug: input.organizationSlug,
      environmentId: source.environmentId,
      resourceIds: [source.attempt.environmentResourceId],
    });
    const mismatch = getDestructiveVolumeReviewMismatch({
      reviewed: [input.review],
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

  const retried = yield* retryDestructiveVolumeAttemptRecord({
    attemptId: input.attemptId,
    target: input.review.target,
    evidence: input.review.evidence,
  });
  if (
    retried.mode === "new_removal" ||
    retried.mode === "reobserve_operation"
  ) {
    yield* dispatchDestructiveVolumeAttempt(retried.attempt.id);
  }
  if (
    retried.mode === "reconcile_tombstone" &&
    retried.attempt.disposition !== "completed"
  ) {
    yield* completeDestructiveVolumeAttempt({
      attemptId: retried.attempt.id,
      operationId: retried.operationId,
    });
  }
  return retried.txid === undefined
    ? { state: "created" as const }
    : {
        txid: retried.txid,
        data: { state: "created" as const },
      };
});

function parseEnvironmentChangeStateNode(input: {
  readonly nodeType: "service" | "variable_group" | "volume";
  readonly nodeId: string;
  readonly nodeLineageId: string;
  readonly revisionId: string | null;
  readonly config: unknown;
}) {
  const identity = {
    nodeId: input.nodeId,
    nodeLineageId: input.nodeLineageId,
    revisionId: input.revisionId,
  };
  const invalid = () =>
    new Conflict({ message: "Environment change state is invalid." });
  if (input.nodeType === "service") {
    return Schema.decodeUnknownEffect(serviceDeploymentConfigSchema)(
      input.config,
      strictParseOptions,
    ).pipe(
      Effect.map(
        (config): EnvironmentChangeStateNodeProjection => ({
          ...identity,
          nodeType: "service",
          config,
        }),
      ),
      Effect.mapError(invalid),
    );
  }
  const nodeType = input.nodeType;
  return decodeEnvironmentResourceNodeConfig(nodeType, input.config).pipe(
    Effect.map((decoded) => ({
      ...identity,
      ...decoded,
    })),
    Effect.mapError(invalid),
  );
}

type EnvironmentChangeStateEvidenceNode = NonNullable<
  EnvironmentChangeStateProjection["deploymentEvidence"]
>["nodes"][number];

function parseEvidenceChangeStateNode(input: {
  readonly nodeType: "service" | "variable_group" | "volume";
  readonly nodeId: string;
  readonly nodeLineageId: string;
  readonly revisionId: string | null;
  readonly config: unknown;
}): Effect.Effect<EnvironmentChangeStateEvidenceNode, Conflict> {
  return input.config === null
    ? Effect.succeed({ ...input, config: null })
    : parseEnvironmentChangeStateNode(input);
}

const projectEnvironmentChangeStateRecords = Effect.fn(
  "Deployments.projectEnvironmentChangeStateRecords",
)(function* (projection: EnvironmentSnapshotProjection) {
  return yield* Effect.forEach(projection.explicitStates, (state) =>
    Effect.gen(function* () {
      const savedNodes = yield* Effect.forEach(
        state.saved?.nodes ?? [],
        parseEnvironmentChangeStateNode,
      );
      const appliedNodes = yield* Effect.forEach(
        state.applied.nodes,
        parseEnvironmentChangeStateNode,
      );
      const evidenceNodes = yield* Effect.forEach(
        state.deploymentEvidence?.nodes ?? [],
        parseEvidenceChangeStateNode,
      );
      return {
        environmentId: state.environmentId,
        saved: state.saved ? { ...state.saved, nodes: savedNodes } : null,
        applied: { ...state.applied, nodes: appliedNodes },
        deploymentEvidence: state.deploymentEvidence
          ? { ...state.deploymentEvidence, nodes: evidenceNodes }
          : null,
      };
    }),
  );
});

export const listLatestOrganizationEnvironmentChangeStates = Effect.fn(
  "Deployments.listLatestOrganizationEnvironmentChangeStates",
)(function* (
  actor: Actor,
  input: OrganizationEnvironmentChangeStateQueryInput,
) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  const projection = yield* loadEnvironmentSnapshotProjection({
      kind: "organization",
      organizationId: organization.id,
    });
  return yield* projectEnvironmentChangeStateRecords(projection);
});

export const listDeploymentOperationEvidence = Effect.fn(
  "Deployments.listDeploymentOperationEvidence",
)(function* (actor: Actor, input: DeploymentOperationEvidencePageQueryInput) {
  const organization = yield* requireOrganization(actor, input.organizationSlug);
  return yield* loadAuthorizedDeploymentEvidence({
      organizationId: organization.id,
      deploymentId: input.deploymentId,
      afterSequence: input.afterSequence,
      limit: input.limit ?? 50,
    });
});

const requirePullableManualSdkDeploy = Effect.fn(
  "Deployments.requirePullableManualSdkDeploy",
)(function* (environmentId: string) {
  const { drizzle: database } = yield* Database;
  const services = yield* database
        .select({
          id: schemaService.id,
          name: schemaService.name,
          source: schemaService.sourceConfig,
        })
        .from(schemaService)
        .where(
          and(
            eq(schemaService.environmentId, environmentId),
            isNull(schemaService.deletedAt),
          ),
        );
  return yield* requirePullableSdkDeployImagesEffect(services);
});

export const createEnvironmentDeploymentSnapshot = Effect.fn(
  "Deployments.createEnvironmentDeploymentSnapshot",
)(function* (actor: Actor, input: CreateEnvironmentDeploymentSnapshotInput) {
  const context = yield* requireEnvironment(actor, input);
  const shouldDeploy = input.deploy !== false;
  if (shouldDeploy) {
    yield* requirePullableManualSdkDeploy(context.environment.id);
  }
  const message = input.message?.trim() || null;
  if (shouldDeploy) {
    const attempt = yield* withMutationReceipt(
      createManualEnvironmentDeployment({
        environmentId: context.environment.id,
        actorId: actor.userId,
        message,
        review: {
          savedStateBasis: input.savedStateBasis,
          workingStateFingerprint: input.reviewedWorkingStateFingerprint,
          destructiveServiceIds: [],
          destructiveVolumeReviews: [],
        },
      }),
      { isolationLevel: "read committed" },
    ).pipe(
      Effect.catchIf(isActiveDeploymentUniqueViolation, () =>
        new Validation({
          field: "environmentId",
          message: "An environment deployment attempt is already active.",
        }),
      ),
    );
    yield* dispatchEnvironmentDeployment({
      environmentDeploymentId: attempt.data.environmentDeploymentId,
      environmentId: context.environment.id,
    });
    return { state: "deployment_queued" as const, txid: attempt.txid };
  }

  const destructiveVolumeReviews = input.destructiveVolumeReviews ?? [];
  const reviewedDestructiveSave = {
    serviceIds: input.destructiveServiceIds ?? [],
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

  const saved = yield* withMutationReceipt(
    saveReviewedEnvironmentState({
      environmentId: context.environment.id,
      actorId: actor.userId,
      message,
      review: {
        savedStateBasis: input.savedStateBasis,
        workingStateFingerprint: input.reviewedWorkingStateFingerprint,
        destructiveServiceIds: reviewedDestructiveSave.serviceIds,
        destructiveVolumeReviews,
      },
    }),
    { isolationLevel: "read committed" },
  );
  return { state: "saved" as const, txid: saved.txid };
});

export const discardEnvironmentSavedChange = Effect.fn(
  "Deployments.discardEnvironmentSavedChange",
)(function* (actor: Actor, input: DiscardEnvironmentSavedChangeInput) {
  const context = yield* requireEnvironment(actor, input);
  return yield* withMutationReceipt(
    discardEnvironmentSavedState({
      environmentId: context.environment.id,
      actorId: actor.userId,
      command: input.command,
    }),
    { isolationLevel: "repeatable read" },
  );
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

export const confirmEnvironmentDeployment = Effect.fn(
  "Deployments.confirmEnvironmentDeployment",
)(function* (actor: Actor, input: ConfirmEnvironmentDeploymentInput) {
  const { drizzle: database } = yield* Database;
  const context = yield* requireEnvironment(actor, input);
  const rows = yield* database
        .select({
          id: schemaEnvironmentDeployment.id,
          status: schemaEnvironmentDeployment.status,
          deployPreview: schemaEnvironmentDeployment.deployPreview,
        })
        .from(schemaEnvironmentDeployment)
        .where(
          and(
            eq(schemaEnvironmentDeployment.id, input.environmentDeploymentId),
            eq(schemaEnvironmentDeployment.environmentId, context.environment.id),
          ),
        )
        .limit(1);
  const deployment = rows[0];
  if (deployment === undefined) {
    return yield* new Conflict({
      message: "The deployment is not active or does not exist.",
    });
  }
  yield* requireConfirmableSdkDeployPreview({
    status: deployment.status,
    preview: deployment.deployPreview,
  });
  yield* sendInngestEvent(
    createEnvironmentDeployConfirmedEvent({
        environmentDeploymentId: deployment.id,
      }),
  );
  return { state: "confirmed" as const };
});
