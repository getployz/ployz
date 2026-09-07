import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";

export type EnvironmentDeployVolume = {
  volumeResourceId: string;
};

export type EnvironmentDeploymentApplyResult = {
  coreDeployId: string | null;
};

export type EnvironmentDeploySnapshot = {
  serviceId: string;
  serviceSlug: string;
  config: ServiceDeploymentConfig;
  replicas?: number;
  resolvedEnv?: Record<string, string>;
  healthcheckPort?: number;
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

export class UnsupportedDeploymentSourceError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "UnsupportedDeploymentSourceError";
  }
}

export function getResolvedHealthcheckPort(values: Record<string, string> | undefined) {
  const port = Number(values?.["PORT"]);
  return Number.isInteger(port) && port > 0 && port <= 65_535 ? port : undefined;
}
