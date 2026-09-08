import { Schema } from "effect";
import { ENVIRONMENT_DEPLOYMENT_STATUSES } from "#/modules/deployments/tables";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { VariableGroupConfig } from "#/modules/environment-design/variable-group-config";
import type { VolumeConfig } from "#/modules/environment-design/volume-config";
import {
  destructiveVolumeReviewSchema,
  destructiveVolumeReviewsSchema,
  reviewedDestructiveVolumeEvidenceSchema,
  reviewedDestructiveVolumeTargetSchema,
  type DestructiveVolumeReview,
} from "#/modules/environment-design/destructive-volume-review";
import {
  environmentSavedStateBasisSchema,
  environmentSavedStateDiscardCommandSchema,
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

export const prepareDestructiveVolumeRetrySchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  attemptId: Uuid,
});

export const retryDestructiveVolumeAttemptSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  attemptId: Uuid,
  review: destructiveVolumeReviewSchema,
});

const destructiveVolumeDispositionSchema = Schema.Literals([
  "active",
  "accepted",
  "completed",
  "partial",
  "core_terminal",
  "cloud_timeout",
  "cloud_cancelled",
  "failed",
]);

const destructiveVolumeAttemptSummarySchema = Schema.Struct({
  id: Uuid,
  environmentDeploymentId: Uuid,
  environmentResourceId: Uuid,
  retryOfAttemptId: Schema.NullOr(Uuid),
  target: reviewedDestructiveVolumeTargetSchema,
  evidence: reviewedDestructiveVolumeEvidenceSchema,
  evidenceFingerprint: NonEmptyString,
  disposition: destructiveVolumeDispositionSchema,
  operationId: Schema.NullOr(Schema.String),
  startSequence: Schema.NullOr(Schema.String),
  inngestRunId: Schema.NullOr(Schema.String),
  requestPublishedAt: Schema.NullOr(Schema.Date),
  acceptedAt: Schema.NullOr(Schema.Date),
  terminalEvent: Schema.NullOr(Schema.Record(Schema.String, Schema.Json)),
  failure: Schema.NullOr(
    Schema.Struct({ code: Schema.String, message: Schema.String }),
  ),
  deadlineAt: Schema.NullOr(Schema.Date),
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

export const discardEnvironmentSavedChangeSchema = Schema.Struct({
  ...EnvironmentContext,
  command: environmentSavedStateDiscardCommandSchema,
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
  canRetry: Schema.Boolean,
  failureCode: Schema.NullOr(Schema.String),
  dispatchRequestedAt: Schema.NullOr(Schema.Date),
  startedAt: Schema.NullOr(Schema.Date),
  finishedAt: Schema.NullOr(Schema.Date),
  createdAt: Schema.Date,
  updatedAt: Schema.Date,
  serviceCount: finiteNumber({ integer: true, minimum: 0 }),
  projectSlug: ProjectSlug,
  environmentSlug: EnvironmentSlug,
  destructiveVolumeAttempts: Schema.mutable(
    Schema.Array(destructiveVolumeAttemptSummarySchema),
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
  | { state: "deployment_queued"; txid: number }
  | Extract<
      DestructiveVolumeSubmissionOutcome,
      { state: "review_updated_evidence" }
    >;
export type OrganizationEnvironmentChangeStateQueryInput =
  typeof organizationEnvironmentChangeStateQuerySchema.Type;
export type DiscardEnvironmentSavedChangeInput =
  typeof discardEnvironmentSavedChangeSchema.Type;
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
