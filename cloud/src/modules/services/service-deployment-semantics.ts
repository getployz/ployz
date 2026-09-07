import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import type {
  RuntimeServiceRecord,
  RuntimeStatus,
} from "#/modules/runtime/runtime";

export type ServiceDeploymentSurfaceState =
  | "success"
  | "changed"
  | "warning"
  | "destructive"
  | undefined;

export type ServiceDeploymentSemanticInput = {
  isEmpty: boolean;
  hasBeenDeployed: boolean;
  currentDiffRowCount: number;
  latestDeploymentDiffRowCount: number;
  hasRecordedTargetSnapshot: boolean;
  latestDeploymentStatus: EnvironmentDeploymentStatus | null;
  runtime: RuntimeServiceRecord | null;
  runtimeIsLoading?: boolean;
  clusterStatus: RuntimeStatus;
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
  const latestAttemptChangesDeployedState =
    input.latestDeploymentDiffRowCount > 0;
  const isDeploying =
    input.latestDeploymentStatus != null &&
    ACTIVE_DEPLOYMENT_STATUSES.has(input.latestDeploymentStatus) &&
    (latestAttemptChangesDeployedState || input.currentDiffRowCount > 0);
  const lastDeployFailed =
    input.latestDeploymentStatus === "failed" &&
    latestAttemptChangesDeployedState;

  if (
    input.hasBeenDeployed &&
    !input.isEmpty &&
    input.clusterStatus === "live"
  ) {
    if (!input.runtime) {
      if (input.runtimeIsLoading) {
        return {
          state: undefined,
          statusText: "Connecting…",
          showNewBadge: false,
        };
      }

      return {
        state: "destructive",
        statusText: "Missing from cluster",
        showNewBadge: false,
      };
    }

    if (input.runtime.instanceCount === 0) {
      return {
        state: "destructive",
        statusText: "No replicas",
        showNewBadge: false,
      };
    }
  }

  if (lastDeployFailed) {
    return {
      state: "destructive",
      statusText: "Deploy failed",
      showNewBadge: false,
    };
  }

  if (isDeploying) {
    return {
      state: "changed",
      statusText: "Deploying…",
      showNewBadge: false,
    };
  }

  if (hasEditsAfterCancelledAttempt) {
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

  if (input.currentDiffRowCount > 0) {
    return {
      state: "changed",
      statusText: `${input.currentDiffRowCount} ${
        input.currentDiffRowCount === 1 ? "change" : "changes"
      }`,
      showNewBadge: false,
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

  if (input.clusterStatus === "disabled") {
    return {
      state: undefined,
      statusText: "No servers",
      showNewBadge: false,
    };
  }

  if (input.clusterStatus === "connecting") {
    return {
      state: undefined,
      statusText: "Connecting…",
      showNewBadge: false,
    };
  }

  if (input.clusterStatus === "error") {
    return {
      state: undefined,
      statusText: "Cluster unreachable",
      showNewBadge: false,
    };
  }

  if (!input.runtime) {
    return {
      state: undefined,
      statusText: "Missing from cluster",
      showNewBadge: false,
    };
  }

  const total = input.runtime.instanceCount;
  const ready = input.runtime.readyInstanceCount;

  if (total === 0) {
    return {
      state: undefined,
      statusText: "Stopped",
      showNewBadge: false,
    };
  }

  if (ready < total) {
    return {
      state: "warning",
      statusText: `${ready} of ${total} ready`,
      showNewBadge: false,
    };
  }

  return {
    state: undefined,
    statusText: `${total} ${total === 1 ? "replica" : "replicas"}`,
    showNewBadge: false,
  };
}
