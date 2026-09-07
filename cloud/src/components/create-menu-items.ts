import type { ComponentType, SVGProps } from "react";
import {
  DatabaseIcon,
  HardDriveIcon,
  PackageIcon,
  SquareTerminalIcon,
} from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";

export type CreateMenuItemId =
  | "git-repository"
  | "container-image"
  | "empty-service"
  | "variable-group"
  | "volume"
  | "empty-project";

export type CreateMenuItem = {
  id: CreateMenuItemId;
  icon: ComponentType<SVGProps<SVGSVGElement>>;
  label: string;
};

export const SERVICE_CREATE_MENU_ITEMS: CreateMenuItem[] = [
  { id: "git-repository", icon: GitHubMarkIcon, label: "GitHub repository" },
  { id: "container-image", icon: PackageIcon, label: "Docker image" },
  { id: "empty-service", icon: SquareTerminalIcon, label: "Empty service" },
  { id: "variable-group", icon: DatabaseIcon, label: "Variable Group" },
  { id: "volume", icon: HardDriveIcon, label: "Volume" },
];

const EMPTY_PROJECT_CREATE_MENU_ITEM: CreateMenuItem = {
  id: "empty-project",
  icon: SquareTerminalIcon,
  label: "Empty project",
};

export function getCreateMenuItems({
  includeEmptyProject = false,
}: {
  includeEmptyProject?: boolean;
} = {}) {
  return includeEmptyProject
    ? [...SERVICE_CREATE_MENU_ITEMS, EMPTY_PROJECT_CREATE_MENU_ITEM]
    : SERVICE_CREATE_MENU_ITEMS;
}
