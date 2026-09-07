import {
  DatabaseIcon,
  HardDriveIcon,
  PackageIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
} from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import type { ServiceDeploymentDiffKind } from "#/modules/services/service-deployment-diff/fields";
import type { ServiceRecord } from "#/modules/environment-design/services";
import type { CanvasNodeDiffGroup } from "#/modules/environment-design/canvas-node-diff";

export function getKindIcon(kind: ServiceDeploymentDiffKind) {
  if (kind === "add") {
    return <PlusIcon />;
  }

  if (kind === "remove") {
    return <Trash2Icon />;
  }

  return <PencilIcon />;
}

export function getKindBadgeVariant(kind: ServiceDeploymentDiffKind) {
  if (kind === "add") {
    return "success" as const;
  }

  if (kind === "remove") {
    return "destructive" as const;
  }

  return "changed" as const;
}

export function getKindTextClassName(kind: ServiceDeploymentDiffKind) {
  if (kind === "add") {
    return "text-success";
  }

  if (kind === "remove") {
    return "text-destructive";
  }

  return "text-changed";
}

function getServiceIcon(type: ServiceRecord["source"]["type"]) {
  switch (type) {
    case "empty":
      return <PencilIcon />;
    case "git":
      return <GitHubMarkIcon />;
    case "image":
      return <PackageIcon />;
  }
}

export function getCanvasNodeIcon(group: CanvasNodeDiffGroup) {
  if (group.nodeType === "service") {
    return getServiceIcon(
      group.serviceSourceType ?? "empty",
    );
  }

  if (group.nodeType === "volume") {
    return <HardDriveIcon />;
  }

  return <DatabaseIcon />;
}

export function getServiceChangeAction(kind: ServiceDeploymentDiffKind) {
  if (kind === "add") {
    return "will be added";
  }

  if (kind === "remove") {
    return "will be removed";
  }

  return "will be updated";
}

export function getSettingsLabel(count: number) {
  return count === 1 ? "1 Setting" : `${count} Settings`;
}
