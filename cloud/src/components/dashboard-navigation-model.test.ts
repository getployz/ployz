import { describe, expect, it } from "vitest";

import {
  createDashboardNavItems,
  getDashboardDestination,
  getDashboardProjectDestination,
  getDashboardSectionFromRouteId,
} from "#/components/dashboard-navigation-model";

describe("dashboard navigation model", () => {
  it("renders one stable flat menu at all-project scope", () => {
    const items = createDashboardNavItems({
      kind: "all",
      organizationSlug: "acme",
    });

    expect(items.map((item) => item.label)).toEqual([
      "Overview",
      "Deployments",
      "Logs",
      "Servers",
      "Server Settings",
      "Billing",
    ]);
    expect(items.map((item) => item.to)).toEqual([
      "/cloud/$organizationSlug/~",
      "/cloud/$organizationSlug/~/deployments",
      "/cloud/$organizationSlug/~/logs",
      "/cloud/$organizationSlug/~/servers",
      "/cloud/$organizationSlug/~/settings",
      "/cloud/$organizationSlug/~/billing",
    ]);
  });

  it("adds environment settings when an environment is selected", () => {
    const items = createDashboardNavItems({
      kind: "environment",
      organizationSlug: "acme",
      projectSlug: "storefront",
      environmentSlug: "production",
    });

    expect(items.map((item) => item.label)).toEqual([
      "Overview",
      "Deployments",
      "Logs",
      "Environment Settings",
      "Servers",
      "Server Settings",
      "Billing",
    ]);
    expect(items[1]).toMatchObject({
      to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/deployments",
      params: {
        organizationSlug: "acme",
        projectSlug: "storefront",
        environmentSlug: "production",
      },
    });
    expect(items[4]).toMatchObject({
      to: "/cloud/$organizationSlug/~/servers",
      params: { organizationSlug: "acme" },
    });
    expect(items[5]).toMatchObject({
      to: "/cloud/$organizationSlug/~/settings",
      params: { organizationSlug: "acme" },
    });
  });

  it("preserves the selected section when changing scope", () => {
    expect(
      getDashboardDestination(
        { kind: "all", organizationSlug: "acme" },
        "logs",
      ),
    ).toEqual({
      to: "/cloud/$organizationSlug/~/logs",
      params: { organizationSlug: "acme" },
    });

    expect(
      getDashboardDestination(
        { kind: "all", organizationSlug: "acme" },
        "environment-settings",
      ),
    ).toEqual({
      to: "/cloud/$organizationSlug/~",
      params: { organizationSlug: "acme" },
    });

    expect(
      getDashboardDestination(
        {
          kind: "environment",
          organizationSlug: "acme",
          projectSlug: "storefront",
          environmentSlug: "production",
        },
        "environment-settings",
      ),
    ).toEqual({
      to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
      params: {
        organizationSlug: "acme",
        projectSlug: "storefront",
        environmentSlug: "production",
      },
    });

    expect(
      getDashboardProjectDestination(
        {
          kind: "environment",
          organizationSlug: "acme",
          projectSlug: "storefront",
          environmentSlug: "production",
        },
        "servers",
      ),
    ).toEqual({
      to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug",
      params: {
        organizationSlug: "acme",
        projectSlug: "storefront",
        environmentSlug: "production",
      },
    });
  });

  it("derives active sections from route IDs rather than URL positions", () => {
    expect(
      getDashboardSectionFromRouteId(
        "/_protected/cloud/$organizationSlug/_org/~/deployments",
      ),
    ).toBe(
      "deployments",
    );
    expect(
      getDashboardSectionFromRouteId(
        "/_protected/cloud/$organizationSlug/_org/~/settings",
      ),
    ).toBe("server-settings");
    expect(
      getDashboardSectionFromRouteId(
        "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/settings",
      ),
    ).toBe("environment-settings");
    expect(
      getDashboardSectionFromRouteId(
        "/_protected/cloud/$organizationSlug/_org/~/servers/",
      ),
    ).toBe(
      "servers",
    );
    expect(
      getDashboardSectionFromRouteId(
        "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId",
      ),
    ).toBe("overview");
  });
});
