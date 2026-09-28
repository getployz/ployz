import type { LucideIcon } from "lucide-react";
import { linkOptions, type RegisteredRouter } from "@tanstack/react-router";
import {
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

/** An Environment's four places, in rail order. Architecture is its canvas. */
const environmentPlaceOrder = ["architecture", "deployments", "logs", "settings"] as const;
type EnvironmentPlace = (typeof environmentPlaceOrder)[number];
const environmentPlaces = {
  architecture: {
    label: "Architecture",
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

/** The organization's three places, in rail order. */
const organizationPlaceOrder = ["projects", "servers", "organization-settings"] as const;
type OrganizationDestination = (typeof organizationPlaceOrder)[number] | "billing";
const organizationDestinations = {
  projects: { label: "Projects", icon: LayoutGridIcon, path: "/cloud/$organizationSlug/~" },
  servers: { label: "Servers", icon: ServerIcon, path: "/cloud/$organizationSlug/~/servers" },
  "organization-settings": { label: "Settings", icon: SlidersHorizontalIcon, path: "/cloud/$organizationSlug/~/settings" },
  // A section of Settings, with its own page. Only Ployz-hosted Cloud has billing.
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
  /** Its page's sections, in order; empty for a page without any. */
  sections: DashboardNavSection[];
};

export type DashboardNavSection = ReturnType<typeof placeSections>[number];

/** The search keys that pick a Settings page's section. */
export interface DashboardSectionSearch {
  scope?: "environment" | "project";
  section?: "general" | "builds";
}

/** Where `section` lives in `scope`; a section the scope lacks falls back to its home (Architecture or Projects). */
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
      : environmentPlaces.architecture.path,
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

/** The open section of a page with sections: Settings' `scope`, Organization Settings' `section`, or Billing. */
function openSection(section: DashboardSection, search: DashboardSectionSearch) {
  switch (section) {
    case "settings":
      return search.scope ?? "environment";
    case "organization-settings":
      return search.section ?? "general";
    case "billing":
      return "billing";
    default:
      return undefined;
  }
}

/** The sections of `place`'s page. A page's first section is its plain URL. */
function placeSections(
  scope: DashboardScope,
  place: DashboardSection,
  open: ReturnType<typeof openSection>,
  billingEnabled: boolean,
) {
  if (scope.kind === "environment" && place === "settings") {
    const params = { organizationSlug: scope.organizationSlug, projectSlug: scope.projectSlug, environmentSlug: scope.environmentSlug };
    const to = environmentPlaces.settings.path;
    return [
      { label: "Environment", current: open === "environment", ...linkOptions({ to, params, search: {} }) },
      { label: "Project", current: open === "project", ...linkOptions({ to, params, search: { scope: "project" as const } }) },
    ];
  }
  if (scope.kind === "all" && place === "organization-settings") {
    const params = { organizationSlug: scope.organizationSlug };
    const to = organizationDestinations["organization-settings"].path;
    return [
      { label: "General", current: open === "general", ...linkOptions({ to, params, search: {} }) },
      { label: "Builds", current: open === "builds", ...linkOptions({ to, params, search: { section: "builds" as const } }) },
      ...billingEnabled
        ? [{ label: "Billing", current: open === "billing", ...linkOptions({ to: organizationDestinations.billing.path, params, search: {} }) }]
        : [],
    ];
  }
  return [];
}

/**
 * The only enumeration of destinations. `places` are the scope's places: the rail's first group and the phone tab
 * bar. On an Environment, `organization` is the way back that the rail adds below them; the organization's Settings
 * stays on organization pages, so the rail never shows two Settings.
 */
export function createDashboardNavigation(
  scope: DashboardScope,
  { section, search = {}, billingEnabled = false }: {
    section: DashboardSection;
    search?: DashboardSectionSearch;
    billingEnabled?: boolean;
  },
) {
  const open = openSection(section, search);
  // Billing is a section of the organization's Settings, so Settings is the current place there.
  const currentPlace = section === "billing" ? "organization-settings" : section;
  const item = (target: DashboardScope, key: DashboardSection, { label, icon }: Destination): DashboardNavItem => ({
    section: key,
    label,
    icon,
    current: key === currentPlace,
    sections: placeSections(target, key, open, billingEnabled),
    ...getDashboardDestination(target, key),
  });
  const organizationScope = { kind: "all" as const, organizationSlug: scope.organizationSlug };
  const organization = organizationPlaceOrder.map((key) => item(organizationScope, key, organizationDestinations[key]));
  if (scope.kind === "all") return { places: organization, organization: [] };
  return {
    places: environmentPlaceOrder.map((key) => item(scope, key, environmentPlaces[key])),
    organization: organization.filter((place) => place.section !== "organization-settings"),
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

/** The current place from the deepest route: the canvas and its panels are Architecture. */
export function getDashboardSectionFromRouteId(
  routeId?: RegisteredRouteId,
): DashboardSection {
  return (routeId && sectionByRouteId.get(routeId)) || "architecture";
}

/** The canvas's layout: Architecture and every panel over the canvas, Deployments among them, render inside it. */
export const canvasRouteId: RegisteredRouteId = "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas";
