import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";

export type ServiceDeploymentSurfaceState =
  | "success"
  | "changed"
  | "destructive"
  | undefined;

/** Presentation based only on authored changes and Deployment Attempt history.
 * Runtime observations appear separately as direct container evidence. */
export type ServiceDeploymentSemanticInput = {
  isEmpty: boolean;
  hasBeenDeployed: boolean;
  currentDiffRowCount: number;
  hasRecordedTargetSnapshot: boolean;
  latestDeploymentStatus: EnvironmentDeploymentStatus | null;
};

export type ServiceDeploymentSemantics = {
  state: ServiceDeploymentSurfaceState;
  statusText: string;
  showNewBadge: boolean;
};

const DEPLOYMENT_ATTEMPT_STATUSES = new Set<EnvironmentDeploymentStatus>([
  "queued",
  "planning",
  "deploying",
  "applied",
  "failed",
]);

const ACTIVE_DEPLOYMENT_STATUSES = new Set<EnvironmentDeploymentStatus>([
  "queued",
  "planning",
  "deploying",
]);

export function getServiceDeploymentSemantics(
  input: ServiceDeploymentSemanticInput,
): ServiceDeploymentSemantics {
  const hasBeenAttempted =
    input.latestDeploymentStatus != null &&
    DEPLOYMENT_ATTEMPT_STATUSES.has(input.latestDeploymentStatus);
  const hasEditsAfterCancelledAttempt =
    input.latestDeploymentStatus === "cancelled" &&
    input.currentDiffRowCount > 0;
  const isDeploying = input.latestDeploymentStatus != null && ACTIVE_DEPLOYMENT_STATUSES.has(input.latestDeploymentStatus);
  const lastDeployFailed = input.latestDeploymentStatus === "failed";

  if (lastDeployFailed) {
    return {
      state: "destructive",
      statusText: "Deploy failed",
      showNewBadge: false,
    };
  }

  if (isDeploying) {
    return {
      state: input.currentDiffRowCount > 0 ? "changed" : undefined,
      statusText: "Deploying…",
      showNewBadge: false,
    };
  }

  if (hasEditsAfterCancelledAttempt || input.currentDiffRowCount > 0) {
    return {
      state: "changed",
      statusText: `${input.currentDiffRowCount} ${
        input.currentDiffRowCount === 1 ? "change" : "changes"
      }`,
      showNewBadge: false,
    };
  }

  if (!hasBeenAttempted && !input.hasRecordedTargetSnapshot) {
    return {
      state: "success",
      statusText: "Service will be created",
      showNewBadge: true,
    };
  }

  if (input.isEmpty) {
    return {
      state: undefined,
      statusText: "Empty",
      showNewBadge: false,
    };
  }

  if (!input.hasBeenDeployed) {
    return {
      state: undefined,
      statusText: "Service is offline",
      showNewBadge: false,
    };
  }

  return {
    state: undefined,
    statusText: "Deployed",
    showNewBadge: false,
  };
}
