import { deploymentSourcePinsSchema } from "./source-pins";
import { deploymentProgressSchema } from "./deployment-progress";
import { DeploymentTriggerOrigin } from "./deployment";
import { Schema } from "effect";
import { ENVIRONMENT_DEPLOYMENT_STATUSES } from "#/modules/deployments/tables";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import type { VolumeConfig } from "#/modules/environment-design/volume-config";
import type { DestructiveVolumeReview } from "#/modules/environment-design/destructive-volume-review";
import { reviewedEnvironmentPublicationSchema } from "#/modules/environment-design/working-state-review";
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

const PositiveSequence = Schema.String.check(Schema.isPattern(/^[1-9][0-9]*$/u));
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

export const reviewedPublicationSchema = Schema.Struct({
  ...EnvironmentContext,
  message: Schema.optional(DeploymentMessage),
  intent: Schema.Literals(["save", "manual_deploy"]),
  review: reviewedEnvironmentPublicationSchema,
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
    finiteNumber({ integer: true, minimum: 1, maximum: 10_000 }),
  ),
});

export const deploymentBuildTailQuerySchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  deploymentId: Uuid,
});

/** One page of an environment's attempts, newest first; `before` is the last attempt id of the previous page. */
export const environmentDeploymentsQuerySchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  before: Schema.optional(Uuid),
});

/** One page of a node's History. */
export const nodeDeploymentsQuerySchema = Schema.Struct({ ...environmentDeploymentsQuerySchema.fields, nodeId: Uuid });

export const deploymentAttemptQuerySchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  deploymentId: Uuid,
});

export const deploymentServiceVariablesQuerySchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  deploymentId: Uuid,
  serviceId: Uuid,
});

/**
 * The attempt's target node list as plain facts, diffed against Applied State: provisional while the attempt is queued, frozen
 * with the Attempt Target when it starts. Never config: schema changes never touch frozen rows. `name` is a service's private DNS
 * name (its Image Build and runtime service name) or a volume's name.
 */
export const targetNodeListSchema = Schema.Struct({
  version: Schema.Literal(1),
  nodes: Schema.Array(Schema.Struct({
    nodeId: Schema.String,
    nodeType: Schema.Literals(["service", "volume"]),
    name: Schema.String,
    changed: Schema.Boolean,
    removed: Schema.Boolean,
    needsBuild: Schema.Boolean,
    /** What a service's card shows of its source: a Git repository or an image; null for volumes and empty services. */
    source: Schema.NullOr(Schema.Struct({ kind: Schema.Literals(["git", "image"]), label: Schema.String })),
    /** The volume node ids a service mounts. */
    mounts: Schema.Array(Schema.String),
  })),
});
export type TargetNodeList = typeof targetNodeListSchema.Type;

export const environmentDeploymentSummarySchema = Schema.Struct({
  id: Uuid,
  environmentId: Uuid,
  triggerOrigin: DeploymentTriggerOrigin,
  status: Schema.Literals(ENVIRONMENT_DEPLOYMENT_STATUSES),
  message: Schema.NullOr(Schema.String),
  failureMessage: Schema.NullOr(Schema.String),
  inngestRunId: Schema.NullOr(Schema.String),
  coreDeployId: Schema.NullOr(Schema.String),
  deployPreview: Schema.NullOr(runtimeDeployPreviewSchema),
  runtimeProgress: Schema.NullOr(deploymentProgressSchema),
  sourcePins: deploymentSourcePinsSchema,
  targetNodes: targetNodeListSchema,
  canRetry: Schema.Boolean,
  failureCode: Schema.NullOr(Schema.String),
  dispatchRequestedAt: Schema.NullOr(Schema.Date),
  startedAt: Schema.NullOr(Schema.Date),
  finishedAt: Schema.NullOr(Schema.Date),
  cancellationRequestedAt: Schema.NullOr(Schema.Date),
  createdAt: Schema.Date,
  updatedAt: Schema.Date,
  projectSlug: ProjectSlug,
  environmentSlug: EnvironmentSlug,
});

export type EnvironmentChangeStateNodeProjection = {
  [TNodeType in "service" | "volume"]: {
    nodeType: TNodeType;
    nodeId: string;
    nodeLineageId: string;
    revisionId: string | null;
    config: TNodeType extends "service" ? ServiceDeploymentConfig : VolumeConfig;
  };
}["service" | "volume"];

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

export type ReviewedPublicationInput =
  typeof reviewedPublicationSchema.Type;
export type DestructiveVolumeSubmissionOutcome =
  | { state: "created" }
  | {
      state: "review_updated_evidence";
      freshReviews: DestructiveVolumeReview[];
    };
export type EnvironmentPublicationSubmissionOutcome =
  | { state: "saved" }
  | { state: "deployment_queued"; deploymentId: string }
  | { state: "attempt_dispatch_failed" }
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
export type DeploymentBuildTailQueryInput = typeof deploymentBuildTailQuerySchema.Type;
export type EnvironmentDeploymentsQueryInput = typeof environmentDeploymentsQuerySchema.Type;
export type DeploymentAttemptQueryInput = typeof deploymentAttemptQuerySchema.Type;
export type DeploymentServiceVariablesQueryInput = typeof deploymentServiceVariablesQuerySchema.Type;
export type NodeDeploymentsQueryInput = typeof nodeDeploymentsQuerySchema.Type;
export type EnvironmentDeploymentSummary = Omit<
  typeof environmentDeploymentSummarySchema.Type,
  "deployPreview"
> & { deployPreview: SdkDeployPreview | null };

export const cancelEnvironmentDeploymentSchema = Schema.Struct({
  ...EnvironmentContext,
  deploymentId: Uuid,
});
export type CancelEnvironmentDeploymentInput = typeof cancelEnvironmentDeploymentSchema.Type;
