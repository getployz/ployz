import type { DashboardReviewNodeChange } from "#/modules/environment-design/environment-change-set";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";
import {
  getServiceDeploymentDiffRows,
  presentSettingChange,
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

/** Field states for the drawer, read off the node's change group; the Environment Change Set decides what it compares against. */
export function getServiceDeploymentDiffState(change: DashboardReviewNodeChange | null): ServiceDeploymentDiffState {
  const rows = (change?.settings ?? []).map((row) => ({
    ...row,
    ...presentSettingChange("service", row.path, row.before, row.after),
  }));
  const rowsByPath = new Map(rows.map((row) => [row.path, row]));

  return {
    hasChanges: rows.length > 0,
    // Core emits a `source` row only when the source type itself changed.
    sourceTypeChanged: rowsByPath.has(SERVICE_DEPLOYMENT_DIFF_PATHS.source),
    field: (path) => {
      const row =
        rowsByPath.get(path) ??
        (path === SERVICE_DEPLOYMENT_DIFF_PATHS.routes
          ? rows.find((candidate) => candidate.path.startsWith("routes."))
          : undefined);

      return row
        ? {
            changed: true,
            baselineLabel: change?.comparison === "introduction" ? "Introduced" : "Current",
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
