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

    expect(places.map((place) => place.label)).toEqual(["Canvas", "Deployments", "Logs", "Settings"]);
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

  it("offers no Environment places on organization pages", () => {
    expect(createDashboardNavigation(organization, { section: "servers" }).places).toEqual([]);
  });

  it("lists the organization destinations, with Billing only when billing is configured", () => {
    const withBilling = createDashboardNavigation(environment, { section: "canvas", billingEnabled: true }).organization;
    expect(withBilling.map((item) => item.label)).toEqual(["Projects", "Servers", "Server Settings", "Billing"]);
    expect(withBilling.map((item) => item.to)).toEqual([
      "/cloud/$organizationSlug/~",
      "/cloud/$organizationSlug/~/servers",
      "/cloud/$organizationSlug/~/settings",
      "/cloud/$organizationSlug/~/billing",
    ]);
    expect(withBilling.every((item) => !item.current && item.params.organizationSlug === "acme")).toBe(true);

    const selfHosted = createDashboardNavigation(organization, { section: "servers" }).organization;
    expect(selfHosted.map((item) => item.label)).toEqual(["Projects", "Servers", "Server Settings"]);
    expect(selfHosted.find((item) => item.current)?.label).toBe("Servers");
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
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/deployments`)).toBe("deployments");
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/_canvas/deployments/$deploymentId`)).toBe("deployments");
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/_canvas/`)).toBe("canvas");
    expect(getDashboardSectionFromRouteId(`${environmentRoute}/_canvas/services/$serviceId`)).toBe("canvas");
    expect(getDashboardSectionFromRouteId("/_protected/cloud/$organizationSlug/_org/~/")).toBe("projects");
    expect(getDashboardSectionFromRouteId("/_protected/cloud/$organizationSlug/_org/~/billing")).toBe("billing");
  });
});
