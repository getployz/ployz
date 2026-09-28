import { describe, expect, it } from "vitest";

import {
  createDashboardNavigation,
  getDashboardDestination,
  getDashboardSectionFromRouteId,
} from "#/components/dashboard-navigation-model";

const environment = {
  kind: "environment",
  organizationSlug: "acme",
  projectSlug: "storefront",
  environmentSlug: "production",
} as const;
const organization = { kind: "all", organizationSlug: "acme" } as const;

describe("dashboard navigation model", () => {
  it("gives an Environment its four places, with the current one marked", () => {
    const { places } = createDashboardNavigation(environment, { section: "logs" });

    expect(places.map((place) => place.label)).toEqual(["Architecture", "Deployments", "Logs", "Settings"]);
    expect(places.map((place) => place.to)).toEqual([
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments",
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug/logs",
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
    ]);
    expect(places.filter((place) => place.current).map((place) => place.label)).toEqual(["Logs"]);
    expect(places[2]).toMatchObject({
      params: { organizationSlug: "acme", projectSlug: "storefront", environmentSlug: "production" },
      search: {},
    });
  });

  it("gives the organization three places, and an Environment only Projects and Servers of them", () => {
    const { places, organization: none } = createDashboardNavigation(organization, { section: "servers" });
    expect(places.map((place) => place.label)).toEqual(["Projects", "Servers", "Organization"]);
    expect(places.map((place) => place.to)).toEqual([
      "/cloud/$organizationSlug/~",
      "/cloud/$organizationSlug/~/servers",
      "/cloud/$organizationSlug/~/settings",
    ]);
    expect(places.filter((place) => place.current).map((place) => place.label)).toEqual(["Servers"]);
    expect(none).toEqual([]);

    const back = createDashboardNavigation(environment, { section: "logs" }).organization;
    expect(back.map((place) => place.label)).toEqual(["Projects", "Servers"]);
    expect(back.every((place) => !place.current && place.params.organizationSlug === "acme")).toBe(true);
  });

  it("lists Settings' sections, the first at the page's plain URL and the open one marked", () => {
    const sections = (search?: { scope?: "environment" | "project" }, section: "settings" | "logs" = "settings") =>
      createDashboardNavigation(environment, { section, search }).places.find((place) => place.label === "Settings")?.sections;

    expect(sections()?.map(({ label, search, current }) => ({ label, search, current }))).toEqual([
      { label: "Environment", search: {}, current: true },
      { label: "Project", search: { scope: "project" }, current: false },
    ]);
    expect(sections({ scope: "project" })?.find((item) => item.current)?.label).toBe("Project");
    expect(sections({}, "logs")?.some((item) => item.current)).toBe(false);
    expect(createDashboardNavigation(environment, { section: "logs" }).places
      .filter((place) => place.sections.length).map((place) => place.label)).toEqual(["Settings"]);
  });

  it("makes Billing a section of Organization, only when billing is configured", () => {
    const settings = (section: "organization-settings" | "billing", search: { section?: "general" | "builds" }, billingEnabled: boolean) =>
      createDashboardNavigation(organization, { section, search, billingEnabled }).places.find((place) => place.label === "Organization");

    const billing = settings("billing", {}, true);
    expect(billing?.current).toBe(true);
    expect(billing?.sections.map(({ label, to, current }) => ({ label, to, current }))).toEqual([
      { label: "General", to: "/cloud/$organizationSlug/~/settings", current: false },
      { label: "Builds", to: "/cloud/$organizationSlug/~/settings", current: false },
      { label: "Billing", to: "/cloud/$organizationSlug/~/billing", current: true },
    ]);
    expect(settings("organization-settings", { section: "builds" }, true)?.sections.find((item) => item.current)?.label).toBe("Builds");
    expect(settings("organization-settings", {}, false)?.sections.map((item) => item.label)).toEqual(["General", "Builds"]);
  });

  it("keeps a section across scopes where it exists, else falls back to the scope's home", () => {
    expect(getDashboardDestination(organization, "logs")).toEqual({
      to: "/cloud/$organizationSlug/~",
      params: { organizationSlug: "acme" },
      search: {},
    });
    expect(getDashboardDestination({ kind: "all", organizationSlug: "other" }, "servers")).toEqual({
      to: "/cloud/$organizationSlug/~/servers",
      params: { organizationSlug: "other" },
      search: {},
    });
    expect(getDashboardDestination(environment, "billing").to).toBe(
      "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
    );
    expect(getDashboardDestination({ ...environment, environmentSlug: "staging" }, "settings")).toEqual({
      to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
      params: { organizationSlug: "acme", projectSlug: "storefront", environmentSlug: "staging" },
      search: {},
    });
  });

  it("reads the current place from the route", () => {
    const environmentRoute = "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug";
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/logs`)).toBe("logs");
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/settings`)).toBe("settings");
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/_canvas/deployments/`)).toBe("deployments");
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/_canvas/deployments/$deploymentId`)).toBe("deployments");
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/_canvas/`)).toBe("architecture");
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/_canvas/services/$serviceId`)).toBe("architecture");
    expect(getDashboardSectionFromRouteId("/_protected/cloud/$organizationSlug/_org/~/")).toBe("projects");
    expect(getDashboardSectionFromRouteId("/_protected/cloud/$organizationSlug/_org/~/billing")).toBe("billing");
    expect(getDashboardSectionFromRouteId("/_protected/cloud/$organizationSlug/_org/~/servers/$serverId")).toBe("servers");
  });
});
