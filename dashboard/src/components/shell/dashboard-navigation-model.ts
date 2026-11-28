import type { LucideIcon } from "lucide-react";
import { linkOptions, type RegisteredRouter } from "@tanstack/react-router";
import { ENVIRONMENT_DEPLOYMENTS_ROUTE_TO, ENVIRONMENT_INDEX_ROUTE_TO, VERSIONS_ROUTE_TO } from "#/lib/environment-routes";
import {
  Building2Icon,
  ChartLineIcon,
  CircleUserRoundIcon,
  CreditCardIcon,
  GitPullRequestIcon,
  HistoryIcon,
  LayoutGridIcon,
  PlugIcon,
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

/** The pages of Organization settings, in order: the section strip, the `section` search key and the page's body all follow it. */
export const ORGANIZATION_SETTINGS_SECTIONS = [
  { id: "general", label: "General" },
  { id: "network", label: "Network" },
  { id: "members", label: "Members" },
  { id: "tokens", label: "Tokens" },
  { id: "github", label: "GitHub" },
  { id: "builds", label: "Builds" },
] as const;
export type OrganizationSettingsSection = (typeof ORGANIZATION_SETTINGS_SECTIONS)[number]["id"];

/** Architecture is the Environment's canvas. */
const environmentPlaceOrder = ["architecture", "previews", "deployments", "logs", "analytics", "settings"] as const;
type EnvironmentPlace = (typeof environmentPlaceOrder)[number];
const environmentPlaces = {
  architecture: {
    label: "Architecture",
    icon: WorkflowIcon,
    path: ENVIRONMENT_INDEX_ROUTE_TO,
  },
  // A repository's pull requests and their previews: the one that starts from this Environment, else the first.
  previews: {
    label: "Pull requests",
    icon: GitPullRequestIcon,
    path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/previews",
  },
  deployments: {
    label: "Deployments",
    icon: HistoryIcon,
    path: ENVIRONMENT_DEPLOYMENTS_ROUTE_TO,
  },
  logs: {
    label: "Logs",
    icon: TerminalIcon,
    path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/logs",
  },
  analytics: {
    label: "Analytics",
    icon: ChartLineIcon,
    path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/analytics",
  },
  settings: {
    label: "Settings",
    icon: SlidersHorizontalIcon,
    path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
  },
} satisfies Record<EnvironmentPlace, Destination>;

const organizationPlaceOrder = ["projects", "servers", "organization-settings"] as const;
type OrganizationDestination = (typeof organizationPlaceOrder)[number] | "integrations" | "billing" | "account";
const organizationDestinations = {
  projects: { label: "Projects", icon: LayoutGridIcon, path: "/cloud/$organizationSlug/~" },
  servers: { label: "Servers", icon: ServerIcon, path: "/cloud/$organizationSlug/~/servers" },
  // Not "Settings": that always means an Environment's.
  "organization-settings": { label: "Organization", icon: Building2Icon, path: "/cloud/$organizationSlug/~/settings" },
  integrations: { label: "Integrations", icon: PlugIcon, path: "/cloud/$organizationSlug/~/integrations" },
  billing: { label: "Billing", icon: CreditCardIcon, path: "/cloud/$organizationSlug/~/billing" },
  // The user's own settings. No rail entry: the avatar row at the rail's foot is this place.
  account: { label: "Account", icon: CircleUserRoundIcon, path: "/cloud/$organizationSlug/~/account" },
} satisfies Record<OrganizationDestination, Destination>;

export type DashboardSection = EnvironmentPlace | OrganizationDestination;

/** The rail place a page lights up: Integrations and Billing sit under Organization. */
function railPlaceOf(section: DashboardSection): DashboardSection {
  return section === "integrations" || section === "billing" ? "organization-settings" : section;
}

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
  section?: OrganizationSettingsSection;
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

const versionsRouteId: RegisteredRouteId = "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/deployments/versions";

/** The open section of a page with sections: Settings' `scope`, Organization's `section`, Billing, or Deployments' route. */
function openSection(section: DashboardSection, search: DashboardSectionSearch, routeId: RegisteredRouteId | undefined) {
  switch (section) {
    case "deployments":
      return routeId === versionsRouteId ? "versions" : "runs";
    case "settings":
      return search.scope ?? "environment";
    case "organization-settings":
      return search.section ?? "general";
    case "billing":
      return "billing";
    case "integrations":
      return "integrations";
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
  if (scope.kind === "environment" && place === "deployments") {
    const params = { organizationSlug: scope.organizationSlug, projectSlug: scope.projectSlug, environmentSlug: scope.environmentSlug };
    return [
      { label: "Runs", current: open === "runs", ...linkOptions({ to: environmentPlaces.deployments.path, params, search: {} }) },
      { label: "Versions", current: open === "versions", ...linkOptions({ to: VERSIONS_ROUTE_TO, params, search: {} }) },
    ];
  }
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
      ...ORGANIZATION_SETTINGS_SECTIONS.map(({ id, label }) => ({
        label,
        current: open === id,
        ...linkOptions({ to, params, search: id === "general" ? {} : { section: id } }),
      })),
      { label: "Integrations", current: open === "integrations", ...linkOptions({ to: organizationDestinations.integrations.path, params, search: {} }) },
      ...billingEnabled
        ? [{ label: "Billing", current: open === "billing", ...linkOptions({ to: organizationDestinations.billing.path, params, search: {} }) }]
        : [],
    ];
  }
  return [];
}

/**
 * The only enumeration of destinations. `places` are the scope's places: the rail's first group and the phone tab
 * bar. On an Environment, `organization` is the way back that the rail adds below them: Projects and Servers.
 */
export function createDashboardNavigation(
  scope: DashboardScope,
  { section, search = {}, billingEnabled = false, routeId }: {
    section: DashboardSection;
    search?: DashboardSectionSearch;
    billingEnabled?: boolean;
    routeId?: RegisteredRouteId;
  },
) {
  const open = openSection(section, search, routeId);
  const currentPlace = railPlaceOf(section);
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

/** The current place from the deepest route's `staticData.section`: the canvas and its panels are Architecture. */
export function getDashboardSectionFromStaticData(staticData?: { section?: DashboardSection }): DashboardSection {
  return staticData?.section ?? "architecture";
}

/** The canvas's layout: Architecture and every panel over the canvas render inside it. */
export const canvasRouteId: RegisteredRouteId = "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas";
