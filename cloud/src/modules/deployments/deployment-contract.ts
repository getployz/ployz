import { deploymentProgressSchema } from "./deployment-progress";
import { Schema } from "effect";
import { ENVIRONMENT_DEPLOYMENT_STATUSES } from "#/modules/deployments/tables";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { VariableGroupConfig } from "#/modules/environment-design/variable-group-config";
import type { VolumeConfig } from "#/modules/environment-design/volume-config";
import {
  destructiveVolumeReviewsSchema,
} from "#/modules/environment-design/destructive-volume-review";
import type { DestructiveVolumeReview } from "#/modules/environment-design/destructive-volume-review";
import {
  environmentSavedStateBasisSchema,
} from "#/modules/environment-design/saved-state";
import {
  EnvironmentSlug,
  OrganizationSlug,
  ProjectSlug,
  Uuid,
} from "#/modules/environment-design/workspace-schemas";
import { finiteNumber } from "#/modules/environment-design/schema";
import { runtimeDeployPreviewSchema } from "#/modules/deployments/runtime-preview";
import type { SdkDeployPreview } from "#/modules/deployments/runtime-preview";
import { VOLUME_REMOVE_ATTEMPT_STATUSES } from "#/modules/runtime/volume-removal";

export {
  DestructiveVolumeReviewChangedError,
  getDestructiveVolumeReviewMismatch,
  reviewedDestructiveVolumeEvidenceSchema,
  reviewedDestructiveVolumeTargetSchema,
  type DestructiveVolumeReview,
} from "#/modules/environment-design/destructive-volume-review";

const NonEmptyString = Schema.String.check(Schema.isNonEmpty());
const PositiveSequence = Schema.String.check(Schema.isPattern(/^[1-9][0-9]*$/u));
const WorkingStateFingerprint = Schema.String.check(
  Schema.isPattern(/^environment-working-state-v1:[0-9a-f]{64}$/u),
);
const DeploymentMessage = Schema.NullOr(
  Schema.Trim.check(Schema.isMaxLength(500)),
);
const EnvironmentContext = {
  organizationSlug: OrganizationSlug,
  projectSlug: ProjectSlug,
  environmentSlug: EnvironmentSlug,
};

export const prepareEnvironmentDestructiveVolumesSchema = Schema.Struct(
  EnvironmentContext,
);

const volumeRemoveVolumeSummarySchema = Schema.Struct({
  machine_id: NonEmptyString,
  name: NonEmptyString,
});

const volumeRemoveOutcomeSummarySchema = Schema.Struct({
  destroyed: Schema.mutable(Schema.Array(volumeRemoveVolumeSummarySchema)),
  failed: Schema.mutable(
    Schema.Array(
      Schema.Struct({
        ...volumeRemoveVolumeSummarySchema.fields,
        message: Schema.optional(Schema.String),
      }),
    ),
  ),
  omitted: Schema.mutable(Schema.Array(volumeRemoveVolumeSummarySchema)),
});

export const volumeRemoveAttemptSummarySchema = Schema.Struct({
  id: Uuid,
  environmentDeploymentId: Schema.NullOr(Uuid),
  environmentResourceId: Schema.NullOr(Uuid),
  retryOfAttemptId: Schema.NullOr(Uuid),
  volumes: Schema.mutable(Schema.Array(volumeRemoveVolumeSummarySchema)),
  status: Schema.Literals(VOLUME_REMOVE_ATTEMPT_STATUSES),
  inngestRunId: Schema.NullOr(Schema.String),
  outcome: Schema.NullOr(volumeRemoveOutcomeSummarySchema),
  failureMessage: Schema.NullOr(Schema.String),
  startedAt: Schema.NullOr(Schema.Date),
  terminalAt: Schema.NullOr(Schema.Date),
  createdAt: Schema.Date,
  updatedAt: Schema.Date,
});

export const createEnvironmentDeploymentSnapshotSchema = Schema.Struct({
  ...EnvironmentContext,
  message: Schema.optional(DeploymentMessage),
  deploy: Schema.optional(Schema.Boolean),
  savedStateBasis: environmentSavedStateBasisSchema,
  reviewedWorkingStateFingerprint: WorkingStateFingerprint,
  destructiveServiceIds: Schema.optional(
    Schema.mutable(Schema.Array(Uuid)).check(
      Schema.makeFilter((serviceIds) =>
        new Set(serviceIds).size === serviceIds.length
          ? undefined
          : "A destructive Service can be reviewed only once.",
      ),
    ),
  ),
  destructiveVolumeReviews: Schema.optional(destructiveVolumeReviewsSchema),
});

export const organizationEnvironmentChangeStateQuerySchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
});

export const retryEnvironmentDeploymentSchema = Schema.Struct({
  ...EnvironmentContext,
  failedDeploymentId: Uuid,
});

export const dispatchQueuedEnvironmentDeploymentSchema = Schema.Struct(
  EnvironmentContext,
);

export const deploymentOperationEvidencePageQuerySchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  deploymentId: Uuid,
  afterSequence: Schema.optional(PositiveSequence),
  limit: Schema.optional(
    finiteNumber({ integer: true, minimum: 1, maximum: 100 }),
  ),
});

export const environmentDeploymentSummarySchema = Schema.Struct({
  id: Uuid,
  status: Schema.Literals(ENVIRONMENT_DEPLOYMENT_STATUSES),
  message: Schema.NullOr(Schema.String),
  failureMessage: Schema.NullOr(Schema.String),
  inngestRunId: Schema.NullOr(Schema.String),
  coreDeployId: Schema.NullOr(Schema.String),
  deployPreview: Schema.NullOr(runtimeDeployPreviewSchema),
  runtimeProgress: Schema.NullOr(deploymentProgressSchema),
  canRetry: Schema.Boolean,
  failureCode: Schema.NullOr(Schema.String),
  dispatchRequestedAt: Schema.NullOr(Schema.Date),
  startedAt: Schema.NullOr(Schema.Date),
  finishedAt: Schema.NullOr(Schema.Date),
  cancellationRequestedAt: Schema.NullOr(Schema.Date),
  createdAt: Schema.Date,
  updatedAt: Schema.Date,
  serviceCount: finiteNumber({ integer: true, minimum: 0 }),
  projectSlug: ProjectSlug,
  environmentSlug: EnvironmentSlug,
  volumeRemoveAttempts: Schema.mutable(
    Schema.Array(volumeRemoveAttemptSummarySchema),
  ),
});

export type EnvironmentChangeStateNodeProjection = {
  [TNodeType in "service" | "variable_group" | "volume"]: {
    nodeType: TNodeType;
    nodeId: string;
    nodeLineageId: string;
    revisionId: string | null;
    config: TNodeType extends "service"
      ? ServiceDeploymentConfig
      : TNodeType extends "variable_group"
        ? VariableGroupConfig
        : VolumeConfig;
  };
}["service" | "variable_group" | "volume"];

export type EnvironmentChangeStateProjection = {
  environmentId: string;
  saved: {
    token: string;
    snapshotId: string;
    createdAt: Date;
    nodes: EnvironmentChangeStateNodeProjection[];
  } | null;
  applied: {
    token: string;
    nodes: EnvironmentChangeStateNodeProjection[];
  };
  deploymentEvidence: {
    id: string;
    savedStateSnapshotId: string;
    status: EnvironmentDeploymentStatus;
    token: string;
    createdAt: Date;
    nodes: Array<
      Omit<EnvironmentChangeStateNodeProjection, "config"> & {
        config: EnvironmentChangeStateNodeProjection["config"] | null;
      }
    >;
  } | null;
};

export type CreateEnvironmentDeploymentSnapshotInput =
  typeof createEnvironmentDeploymentSnapshotSchema.Type;
export type DestructiveVolumeSubmissionOutcome =
  | { state: "created" }
  | {
      state: "review_updated_evidence";
      freshReviews: DestructiveVolumeReview[];
    };
export type EnvironmentPublicationSubmissionOutcome =
  | { state: "saved" }
  | { state: "deployment_queued" }
  | Extract<
      DestructiveVolumeSubmissionOutcome,
      { state: "review_updated_evidence" }
    >;
export type OrganizationEnvironmentChangeStateQueryInput =
  typeof organizationEnvironmentChangeStateQuerySchema.Type;
export type RetryEnvironmentDeploymentInput =
  typeof retryEnvironmentDeploymentSchema.Type;
export type DispatchQueuedEnvironmentDeploymentInput =
  typeof dispatchQueuedEnvironmentDeploymentSchema.Type;
export type DeploymentOperationEvidencePageQueryInput =
  typeof deploymentOperationEvidencePageQuerySchema.Type;
export type EnvironmentDeploymentSummary = Omit<
  typeof environmentDeploymentSummarySchema.Type,
  "deployPreview"
> & { deployPreview: SdkDeployPreview | null };

export const cancelEnvironmentDeploymentSchema = Schema.Struct({
  ...EnvironmentContext,
  deploymentId: Uuid,
});
export type CancelEnvironmentDeploymentInput = typeof cancelEnvironmentDeploymentSchema.Type;
