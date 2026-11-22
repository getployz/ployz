import type { RegisteredRouter } from "@tanstack/react-router";
import { describe, expect, it } from "vitest";

import {
  createDashboardNavigation,
  getDashboardDestination,
  getDashboardSectionFromStaticData,
  type DashboardSection,
} from "#/components/shell/dashboard-navigation-model";

type RegisteredRouteId = RegisteredRouter["routeTree"]["types"]["fileRouteTypes"]["id"];

const environment = {
  kind: "environment",
  organizationSlug: "acme",
  projectSlug: "storefront",
  environmentSlug: "production",
} as const;
const organization = { kind: "all", organizationSlug: "acme" } as const;

describe("dashboard navigation model", () => {
  it("marks the current place of an Environment and links it to that Environment", () => {
    const { places } = createDashboardNavigation(environment, { section: "logs" });

    const current = places.filter((place) => place.current);
    expect(current.map((place) => place.label)).toEqual(["Logs"]);
    expect(current[0]).toMatchObject({
      params: { organizationSlug: "acme", projectSlug: "storefront", environmentSlug: "production" },
      search: {},
    });
  });

  it("marks the organization's current place, and an Environment links back to the organization without Organization settings", () => {
    const { places, organization: none } = createDashboardNavigation(organization, { section: "servers" });
    expect(places.filter((place) => place.current).map((place) => place.label)).toEqual(["Servers"]);
    expect(none).toEqual([]);

    const back = createDashboardNavigation(environment, { section: "logs" }).organization;
    expect(back.map((place) => place.label)).not.toContain("Organization");
    expect(back.length).toBeGreaterThan(0);
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
      .filter((place) => place.sections.length).map((place) => place.label)).toEqual(["Deployments", "Settings"]);
  });

  it("lists Deployments' Runs and Versions, open by the route", () => {
    const environmentRoute = "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug";
    const open = (routeId: RegisteredRouteId, section: DashboardSection = "deployments") =>
      createDashboardNavigation(environment, { section, routeId }).places
        .find((place) => place.label === "Deployments");

    const runs = open(`${environmentRoute}/deployments/`);
    expect(runs?.current).toBe(true);
    expect(runs?.sections.find((item) => item.current)?.label).toBe("Runs");
    expect(open(`${environmentRoute}/deployments/$deploymentId`)?.sections.find((item) => item.current)?.label).toBe("Runs");
    expect(open(`${environmentRoute}/deployments/versions`)?.sections.find((item) => item.current)?.label).toBe("Versions");
    expect(open(`${environmentRoute}/logs`, "logs")?.sections.some((item) => item.current)).toBe(false);
  });

  it("makes Billing a section of Organization, only when billing is configured", () => {
    const settings = (section: "organization-settings" | "billing", search: { section?: "general" | "network" | "members" | "tokens" | "github" | "builds" }, billingEnabled: boolean) =>
      createDashboardNavigation(organization, { section, search, billingEnabled }).places.find((place) => place.label === "Organization");

    const billing = settings("billing", {}, true);
    expect(billing?.current).toBe(true);
    expect(billing?.sections.filter((item) => item.current).map((item) => item.label)).toEqual(["Billing"]);
    const integrations = createDashboardNavigation(organization, { section: "integrations" }).places.find((place) => place.label === "Organization");
    expect(integrations?.current).toBe(true);
    expect(integrations?.sections.find((item) => item.current)?.label).toBe("Integrations");
    expect(settings("organization-settings", { section: "builds" }, true)?.sections.find((item) => item.current)?.label).toBe("Builds");
    expect(settings("organization-settings", { section: "github" }, true)?.sections.find((item) => item.current)?.label).toBe("GitHub");
    expect(settings("organization-settings", { section: "network" }, true)?.sections.find((item) => item.current)?.label).toBe("Network");
    expect(settings("organization-settings", { section: "tokens" }, true)?.sections.find((item) => item.current)?.label).toBe("Tokens");
    expect(settings("organization-settings", {}, false)?.sections.map((item) => item.label)).not.toContain("Billing");
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

  it("reads the current place from the route's static data, and a page without one is Architecture", () => {
    expect(getDashboardSectionFromStaticData({ section: "logs" })).toBe("logs");
    expect(getDashboardSectionFromStaticData({})).toBe("architecture");
    expect(getDashboardSectionFromStaticData(undefined)).toBe("architecture");
  });
});
