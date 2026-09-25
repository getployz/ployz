import type { LucideIcon } from "lucide-react";
import { linkOptions, type RegisteredRouter } from "@tanstack/react-router";
import {
  ActivityIcon,
  CreditCardIcon,
  LayoutGridIcon,
  ServerCogIcon,
  ServerIcon,
  Settings2Icon,
} from "lucide-react";

const sectionOrder = [
  "overview",
  "logs",
  "environment-settings",
  "servers",
  "server-settings",
  "billing",
] as const;

export type DashboardSection = (typeof sectionOrder)[number];

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

type RegisteredPath =
  RegisteredRouter["routeTree"]["types"]["fileRouteTypes"]["to"];
type RegisteredRouteId =
  RegisteredRouter["routeTree"]["types"]["fileRouteTypes"]["id"];

export type DashboardDestination = ReturnType<typeof getDashboardDestination>;

export type DashboardNavItem = DashboardDestination & {
  section: DashboardSection;
  label: string;
  icon: LucideIcon;
};

interface DashboardSectionDefinition {
  label: string;
  icon: LucideIcon;
  allPath?: RegisteredPath;
  environmentPath?: RegisteredPath;
  /** Only Ployz-hosted Cloud has billing. */
  requiresBilling?: true;
}

const sectionDefinitions = {
  overview: {
    label: "Projects",
    icon: LayoutGridIcon,
    allPath: "/cloud/$organizationSlug/~",
    environmentPath:
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
  },
  logs: {
    label: "Logs",
    icon: ActivityIcon,
    environmentPath:
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug/logs",
  },
  "environment-settings": {
    label: "Settings",
    icon: Settings2Icon,
    environmentPath:
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
  },
  servers: {
    label: "Servers",
    icon: ServerIcon,
    allPath: "/cloud/$organizationSlug/~/servers",
  },
  "server-settings": {
    label: "Server Settings",
    icon: ServerCogIcon,
    allPath: "/cloud/$organizationSlug/~/settings",
  },
  billing: {
    label: "Billing",
    icon: CreditCardIcon,
    allPath: "/cloud/$organizationSlug/~/billing",
    requiresBilling: true,
  },
} satisfies Record<DashboardSection, DashboardSectionDefinition>;

export function getDashboardDestination(
  scope: DashboardScope,
  section: DashboardSection,
) {
  const definition = sectionDefinitions[section];

  if (scope.kind === "all") {
    return linkOptions({
      to: "allPath" in definition
        ? definition.allPath
        : sectionDefinitions.overview.allPath,
      params: { organizationSlug: scope.organizationSlug },
      search: {},
    });
  }

  return linkOptions({
    to: "environmentPath" in definition
      ? definition.environmentPath
      : sectionDefinitions.overview.environmentPath,
    params: {
      organizationSlug: scope.organizationSlug,
      projectSlug: scope.projectSlug,
      environmentSlug: scope.environmentSlug,
    },
    search: {},
  });
}

export function getDashboardSectionLabel(
  scope: DashboardScope,
  section: DashboardSection,
) {
  return scope.kind === "environment" && section === "overview"
    ? "Architecture"
    : sectionDefinitions[section].label;
}

export function createDashboardNavItems(
  scope: DashboardScope,
  { billingEnabled = false }: { billingEnabled?: boolean } = {},
): DashboardNavItem[] {
  return sectionOrder.flatMap((section) => {
    const definition = sectionDefinitions[section];

    const available = (scope.kind === "all"
      ? "allPath" in definition
      : "environmentPath" in definition) &&
      (billingEnabled || !("requiresBilling" in definition));

    return !available
      ? []
      : [
          {
            section,
            label: getDashboardSectionLabel(scope, section),
            icon: definition.icon,
            ...getDashboardDestination(scope, section),
          },
        ];
  });
}

interface SectionByRouteId {
  readonly [routeId: string]: DashboardSection | undefined;
}

const sectionByRouteId: SectionByRouteId = {
  "/_protected/cloud/$organizationSlug/_org/~/billing": "billing",
  "/_protected/cloud/$organizationSlug/_org/~/settings": "server-settings",
  "/_protected/cloud/$organizationSlug/_org/~/servers/": "servers",
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/logs":
    "logs",
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/settings":
    "environment-settings",
};

export function getDashboardSectionFromRouteId(
  routeId?: RegisteredRouteId,
): DashboardSection {
  return routeId ? (sectionByRouteId[routeId] ?? "overview") : "overview";
}
