import {
  HardDriveIcon,
  PackageIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
} from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";
import type { ChangeKind } from "@ployz/sdk";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";

export function getKindIcon(kind: ChangeKind) {
  if (kind === "add") {
    return <PlusIcon />;
  }

  if (kind === "remove") {
    return <Trash2Icon />;
  }

  return <PencilIcon />;
}

export function getKindBadgeVariant(kind: ChangeKind) {
  if (kind === "add") {
    return "success" as const;
  }

  if (kind === "remove") {
    return "destructive" as const;
  }

  return "changed" as const;
}

export function getKindTextClassName(kind: ChangeKind) {
  if (kind === "add") {
    return "text-success";
  }

  if (kind === "remove") {
    return "text-destructive";
  }

  return "text-changed-deep";
}

function getServiceIcon(type: NonNullable<ChangeGroup["serviceSourceType"]>) {
  switch (type) {
    case "empty":
      return <PencilIcon />;
    case "git":
      return <GitHubMarkIcon />;
    case "image":
      return <PackageIcon />;
  }
}

export function getCanvasNodeIcon(group: ChangeGroup) {
  if (group.nodeType === "service") {
    return getServiceIcon(
      group.serviceSourceType ?? "empty",
    );
  }

  return <HardDriveIcon />;
}

export function getServiceChangeAction(kind: ChangeKind) {
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
