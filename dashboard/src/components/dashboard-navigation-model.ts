import type { LucideIcon } from "lucide-react";
import { linkOptions, type RegisteredRouter } from "@tanstack/react-router";
import {
  Building2Icon,
  CreditCardIcon,
  HistoryIcon,
  LayoutGridIcon,
  ServerIcon,
  SlidersHorizontalIcon,
  TerminalIcon,
  WorkflowIcon,
} from "lucide-react";

type RegisteredPath =
  RegisteredRouter["routeTree"]["types"]["fileRouteTypes"]["to"];
type RegisteredRouteId =
  RegisteredRouter["routeTree"]["types"]["fileRouteTypes"]["id"];

interface Destination {
  label: string;
  icon: LucideIcon;
  path: RegisteredPath;
}

/** An Environment's four places, in rail order. */
const environmentPlaceOrder = ["canvas", "deployments", "logs", "settings"] as const;
type EnvironmentPlace = (typeof environmentPlaceOrder)[number];
const environmentPlaces = {
  canvas: {
    label: "Canvas",
    icon: WorkflowIcon,
    path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
  },
  deployments: {
    label: "Deployments",
    icon: HistoryIcon,
    path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments",
  },
  logs: {
    label: "Logs",
    icon: TerminalIcon,
    path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/logs",
  },
  settings: {
    label: "Settings",
    icon: SlidersHorizontalIcon,
    path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
  },
} satisfies Record<EnvironmentPlace, Destination>;

/** The organization's pages, in avatar-menu order. */
const organizationOrder = ["projects", "servers", "organization-settings", "billing"] as const;
type OrganizationDestination = (typeof organizationOrder)[number];
const organizationDestinations = {
  projects: { label: "Projects", icon: LayoutGridIcon, path: "/cloud/$organizationSlug/~" },
  servers: { label: "Servers", icon: ServerIcon, path: "/cloud/$organizationSlug/~/servers" },
  // Not plain "Settings": the avatar menu reads as personal, and on an Environment it sits beside the rail's Settings.
  "organization-settings": { label: "Organization Settings", icon: Building2Icon, path: "/cloud/$organizationSlug/~/settings" },
  // Only Ployz-hosted Cloud has billing.
  billing: { label: "Billing", icon: CreditCardIcon, path: "/cloud/$organizationSlug/~/billing" },
} satisfies Record<OrganizationDestination, Destination>;

export type DashboardSection = EnvironmentPlace | OrganizationDestination;

function isEnvironmentPlace(section: DashboardSection): section is EnvironmentPlace {
  return environmentPlaceOrder.some((place) => place === section);
}

export type DashboardScope =
  | {
      kind: "all";
      organizationSlug: string;
    }
  | {
      kind: "environment";
      organizationSlug: string;
      projectSlug: string;
      environmentSlug: string;
    };

export type DashboardDestination = ReturnType<typeof getDashboardDestination>;

export type DashboardNavItem = DashboardDestination & {
  section: DashboardSection;
  label: string;
  icon: LucideIcon;
  current: boolean;
};

/** Where `section` lives in `scope`; a section the scope lacks falls back to its home (Canvas or Projects). */
export function getDashboardDestination(
  scope: DashboardScope,
  section: DashboardSection,
) {
  if (scope.kind === "all") {
    return linkOptions({
      to: isEnvironmentPlace(section)
        ? organizationDestinations.projects.path
        : organizationDestinations[section].path,
      params: { organizationSlug: scope.organizationSlug },
      search: {},
    });
  }

  return linkOptions({
    to: isEnvironmentPlace(section)
      ? environmentPlaces[section].path
      : environmentPlaces.canvas.path,
    params: {
      organizationSlug: scope.organizationSlug,
      projectSlug: scope.projectSlug,
      environmentSlug: scope.environmentSlug,
    },
    search: {},
  });
}

export function getDashboardSectionLabel(section: DashboardSection) {
  return isEnvironmentPlace(section)
    ? environmentPlaces[section].label
    : organizationDestinations[section].label;
}

/**
 * The only enumeration of destinations. `places` are the Environment's four places for the rail and the
 * phone tab bar (none on organization pages); `organization` fills the avatar menu.
 */
export function createDashboardNavigation(
  scope: DashboardScope,
  { section, billingEnabled = false }: { section: DashboardSection; billingEnabled?: boolean },
) {
  const item = (target: DashboardScope, key: DashboardSection, { label, icon }: Destination): DashboardNavItem => ({
    section: key,
    label,
    icon,
    current: key === section,
    ...getDashboardDestination(target, key),
  });
  const organizationScope = { kind: "all" as const, organizationSlug: scope.organizationSlug };
  return {
    places: scope.kind === "all"
      ? []
      : environmentPlaceOrder.map((key) => item(scope, key, environmentPlaces[key])),
    organization: organizationOrder
      .filter((key) => billingEnabled || key !== "billing")
      .map((key) => item(organizationScope, key, organizationDestinations[key])),
  };
}

const sectionByRouteId = new Map<RegisteredRouteId, DashboardSection>([
  ["/_protected/cloud/$organizationSlug/_org/~/", "projects"],
  ["/_protected/cloud/$organizationSlug/_org/~/billing", "billing"],
  ["/_protected/cloud/$organizationSlug/_org/~/settings", "organization-settings"],
  ["/_protected/cloud/$organizationSlug/_org/~/servers/", "servers"],
  ["/_protected/cloud/$organizationSlug/_org/~/servers/$serverId", "servers"],
  ["/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/", "deployments"],
  ["/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/deployments/$deploymentId", "deployments"],
  ["/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/logs", "logs"],
  ["/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/settings", "settings"],
  // Opened from Settings → Project, over the canvas.
  ["/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/pr-environments/$repositoryId", "settings"],
]);

/** The current place from the deepest route: the canvas and its panels are Canvas. */
export function getDashboardSectionFromRouteId(
  routeId?: RegisteredRouteId,
): DashboardSection {
  return (routeId && sectionByRouteId.get(routeId)) || "canvas";
}
