import { restoreServiceSetting } from "@ployz/sdk/config";
import { projectServiceDeploymentConfig, type ServiceDeploymentConfig, type ServiceDeploymentFieldSelection } from "#/modules/environment-design/services";
import type { ServiceDeploymentDiffPath } from "./fields";

export function discardServiceDeploymentDiffPath(input: {
  draft: ServiceDeploymentFieldSelection;
  baseline: ServiceDeploymentConfig;
  path: ServiceDeploymentDiffPath;
}) {
  const { version: _version, env: _env, mounts: _mounts, variableGroupAttachments: _variableGroupAttachments, ...settings } = restoreServiceSetting(
    projectServiceDeploymentConfig(input.draft), input.baseline, input.path,
  );
  Object.assign(input.draft, settings);
}
