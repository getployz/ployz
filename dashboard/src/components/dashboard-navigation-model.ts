import type { LucideIcon } from "lucide-react";
import type { RegisteredRouter } from "@tanstack/react-router";
import {
  ActivityIcon,
  CreditCardIcon,
  LayoutGridIcon,
  RocketIcon,
  ServerCogIcon,
  ServerIcon,
  Settings2Icon,
} from "lucide-react";

const sectionOrder = [
  "overview",
  "deployments",
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

export type DashboardDestination = {
  to: string;
  params: Record<string, string>;
};

export type DashboardNavItem = DashboardDestination & {
  section: DashboardSection;
  label: string;
  icon: LucideIcon;
  group: "scope" | "organization";
};

interface DashboardSectionDefinition {
  label: string;
  icon: LucideIcon;
  group: "scope" | "organization";
  allPath?: RegisteredPath;
  environmentPath?: RegisteredPath;
}

const sectionDefinitions = {
  overview: {
    label: "Overview",
    icon: LayoutGridIcon,
    group: "scope",
    allPath: "/cloud/$organizationSlug/~",
    environmentPath:
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
  },
  deployments: {
    label: "Deployments",
    icon: RocketIcon,
    group: "scope",
    allPath: "/cloud/$organizationSlug/~/deployments",
    environmentPath:
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments",
  },
  logs: {
    label: "Logs",
    icon: ActivityIcon,
    group: "scope",
    allPath: "/cloud/$organizationSlug/~/logs",
    environmentPath:
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug/logs",
  },
  "environment-settings": {
    label: "Environment Settings",
    icon: Settings2Icon,
    group: "scope",
    environmentPath:
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
  },
  servers: {
    label: "Servers",
    icon: ServerIcon,
    group: "organization",
    allPath: "/cloud/$organizationSlug/~/servers",
  },
  "server-settings": {
    label: "Server Settings",
    icon: ServerCogIcon,
    group: "organization",
    allPath: "/cloud/$organizationSlug/~/settings",
  },
  billing: {
    label: "Billing",
    icon: CreditCardIcon,
    group: "organization",
    allPath: "/cloud/$organizationSlug/~/billing",
  },
} satisfies Record<DashboardSection, DashboardSectionDefinition>;

export function getDashboardDestination(
  scope: DashboardScope,
  section: DashboardSection,
): DashboardDestination {
  const definition: DashboardSectionDefinition = sectionDefinitions[section];

  if (scope.kind === "all" || !definition.environmentPath) {
    return {
      to: definition.allPath ?? "/cloud/$organizationSlug/~",
      params: { organizationSlug: scope.organizationSlug },
    };
  }

  return {
    to: definition.environmentPath,
    params: {
      organizationSlug: scope.organizationSlug,
      projectSlug: scope.projectSlug,
      environmentSlug: scope.environmentSlug,
    },
  };
}

export function getDashboardProjectDestination(
  scope: Extract<DashboardScope, { kind: "environment" }>,
  section: DashboardSection,
): DashboardDestination {
  const definition: DashboardSectionDefinition = sectionDefinitions[section];
  const projectSection = definition.environmentPath ? section : "overview";

  return getDashboardDestination(scope, projectSection);
}

export function createDashboardNavItems(
  scope: DashboardScope,
): DashboardNavItem[] {
  return sectionOrder.flatMap((section) => {
    const definition: DashboardSectionDefinition = sectionDefinitions[section];

    return scope.kind === "all" && !definition.allPath
      ? []
      : [
          {
            section,
            label: definition.label,
            icon: definition.icon,
            group: definition.group,
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
  "/_protected/cloud/$organizationSlug/_org/~/deployments": "deployments",
  "/_protected/cloud/$organizationSlug/_org/~/logs": "logs",
  "/_protected/cloud/$organizationSlug/_org/~/settings": "server-settings",
  "/_protected/cloud/$organizationSlug/_org/~/servers/": "servers",
  "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments":
    "deployments",
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
