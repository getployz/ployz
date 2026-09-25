import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";

export type EnvironmentDeployVolume = {
  volumeResourceId: string;
};

export type EnvironmentDeploySnapshot = {
  serviceId: string;
  serviceSlug: string;
  config: ServiceDeploymentConfig;
  replicas?: number;
  resolvedEnv?: Record<string, string>;
};

export const TERMINAL_ENVIRONMENT_DEPLOYMENT_STATUSES =
  new Set<EnvironmentDeploymentStatus>([
    "applied",
    "failed",
    "cancelled",
  ]);

export const ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES =
  new Set<EnvironmentDeploymentStatus>([
    "queued",
    "planning",
    "deploying",
  ]);

/** Queued, planning or deploying: the attempt holds, or waits for, the Environment execution slot. */
export const isActiveDeployment = (status: EnvironmentDeploymentStatus) => ACTIVE_ENVIRONMENT_DEPLOYMENT_STATUSES.has(status);
