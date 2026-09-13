import type {
  ServiceDeploymentFieldSelection,
  ServiceRecord,
} from "#/modules/environment-design/services";
import type { EnvironmentWorkingComparison } from "#/modules/environment-design/environment-change-set";
import {
  projectServiceDeploymentConfig,
  type ServiceDeploymentConfig,
} from "#/modules/environment-design/services";
import {
  getServiceDeploymentDiffRows,
  SERVICE_DEPLOYMENT_DIFF_PATHS,
  type ServiceDeploymentDiffKind,
  type ServiceDeploymentDiffPath,
} from "#/modules/services/service-deployment-diff/fields";

export type ServiceDeploymentFieldState = {
  changed: boolean;
  baselineLabel?: "Current" | "Introduced";
  baselineValue?: string;
  currentValue?: string;
  kind?: ServiceDeploymentDiffKind;
};

export type ServiceDeploymentDiffState = {
  hasChanges: boolean;
  sourceTypeChanged: boolean;
  field: (path: ServiceDeploymentDiffPath) => ServiceDeploymentFieldState;
};

export function getServiceDeploymentDiffState(input: {
  service: Pick<ServiceRecord, "id" | "source"> &
    ServiceDeploymentFieldSelection;
  comparison: EnvironmentWorkingComparison<ServiceDeploymentConfig>;
}): ServiceDeploymentDiffState {
  const current = projectServiceDeploymentConfig(input.service);
  const rows = input.comparison
    ? getServiceDeploymentDiffRows({
        serviceId: input.service.id,
        current,
        baseline: input.comparison.value,
      })
    : [];
  const rowsByPath = new Map(rows.map((row) => [row.path, row]));

  return {
    hasChanges: rows.length > 0,
    sourceTypeChanged:
      input.comparison?.value.source.type != null &&
      input.comparison.value.source.type !== input.service.source.type,
    field: (path) => {
      const row =
        rowsByPath.get(path) ??
        (path === SERVICE_DEPLOYMENT_DIFF_PATHS.routes
          ? rows.find((candidate) => candidate.path.startsWith("routes."))
          : undefined);

      return row
        ? {
            changed: true,
            baselineLabel: input.comparison
              ? input.comparison.role === "baseline"
                ? "Current"
                : "Introduced"
              : undefined,
            baselineValue: row.currentValue,
            currentValue: row.newValue,
            kind: row.kind,
          }
        : { changed: false };
    },
  };
}

export function getServiceDeploymentAttemptDiffRows(input: {
  serviceId: string;
  target: ServiceDeploymentConfig;
  deployed: ServiceDeploymentConfig | null;
}) {
  return getServiceDeploymentDiffRows({
    serviceId: input.serviceId,
    current: input.target,
    baseline: input.deployed,
  });
}
