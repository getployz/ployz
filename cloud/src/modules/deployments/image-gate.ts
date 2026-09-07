import { DeployImageNotPullableError } from "#/modules/deployments/deployment-errors";
import type { ServiceSource } from "#/modules/environment-design/services";

export type SdkDeployImageGateService = {
  id: string;
  name: string;
  source: Pick<ServiceSource, "type">;
};

function compareBlockedServices(
  left: SdkDeployImageGateService,
  right: SdkDeployImageGateService,
) {
  if (left.name !== right.name) return left.name < right.name ? -1 : 1;
  if (left.id !== right.id) return left.id < right.id ? -1 : 1;
  return 0;
}

function blocksSdkDeploy(sourceType: ServiceSource["type"]) {
  switch (sourceType) {
    case "image":
    case "empty":
      return false;
    case "git":
      return true;
    default: {
      const exhaustive: never = sourceType;
      return exhaustive;
    }
  }
}

export function findUnpullableSdkDeployImages(
  services: readonly SdkDeployImageGateService[],
): DeployImageNotPullableError | null {
  const blocked = services.filter((service) =>
    blocksSdkDeploy(service.source.type),
  );
  blocked.sort(compareBlockedServices);
  return blocked.length === 0
    ? null
    : new DeployImageNotPullableError({ services: blocked });
}
