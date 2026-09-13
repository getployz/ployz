import type { ServiceDeploymentDiffKind } from "#/modules/services/service-deployment-diff/fields";

export function getKindState(kind: ServiceDeploymentDiffKind) {
  if (kind === "add") {
    return "success" as const;
  }

  if (kind === "remove") {
    return "destructive" as const;
  }

  return "changed" as const;
}
