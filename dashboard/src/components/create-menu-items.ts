import type { ComponentType, SVGProps } from "react";
import {
  DatabaseIcon,
  FolderIcon,
  HardDriveIcon,
  PackageIcon,
  SquareTerminalIcon,
} from "lucide-react";
import { GitHubMarkIcon } from "#/components/icons/github-mark";

export type CreateMenuItemId =
  | "git-repository"
  | "container-image"
  | "database"
  | "empty-service"
  | "volume"
  | "config"
  | "empty-project";

export type CreateMenuItem = {
  id: CreateMenuItemId;
  icon: ComponentType<SVGProps<SVGSVGElement>>;
  label: string;
  /** The create panel it opens; without one, choosing it creates at once. */
  panel?: "git" | "image" | "database";
};

/** A panel of the create command: the menu, or one a menu item opens. */
export type CreatePanel = "root" | NonNullable<CreateMenuItem["panel"]>;

export const SERVICE_CREATE_MENU_ITEMS: CreateMenuItem[] = [
  { id: "git-repository", icon: GitHubMarkIcon, label: "GitHub repository", panel: "git" },
  { id: "container-image", icon: PackageIcon, label: "Docker image", panel: "image" },
  { id: "database", icon: DatabaseIcon, label: "Database", panel: "database" },
  { id: "empty-service", icon: SquareTerminalIcon, label: "Empty service" },
  { id: "volume", icon: HardDriveIcon, label: "Volume" },
  { id: "config", icon: FolderIcon, label: "Config" },
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
