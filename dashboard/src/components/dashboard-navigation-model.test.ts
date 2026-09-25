import { describe, expect, it } from "vitest";

import {
  createDashboardNavItems,
  getDashboardDestination,
  getDashboardSectionFromRouteId,
} from "#/components/dashboard-navigation-model";

describe("dashboard navigation model", () => {
  it("shows only organization destinations at organization scope", () => {
    const items = createDashboardNavItems({
      kind: "all",
      organizationSlug: "acme",
    }, { billingEnabled: true });

    expect(items.map((item) => item.label)).toEqual([
      "Projects",
      "Servers",
      "Server Settings",
      "Billing",
    ]);
    expect(items.map((item) => item.to)).toEqual([
      "/cloud/$organizationSlug/~",
      "/cloud/$organizationSlug/~/servers",
      "/cloud/$organizationSlug/~/settings",
      "/cloud/$organizationSlug/~/billing",
    ]);
  });

  it("shows Architecture and only environment destinations in an environment", () => {
    const items = createDashboardNavItems({
      kind: "environment",
      organizationSlug: "acme",
      projectSlug: "storefront",
      environmentSlug: "production",
    });

    expect(items.map((item) => item.label)).toEqual([
      "Architecture",
      "Logs",
      "Settings",
    ]);
    expect(items[1]).toMatchObject({
      to: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/logs",
      params: {
        organizationSlug: "acme",
        projectSlug: "storefront",
        environmentSlug: "production",
      },
    });
    expect(items.every((item) => item.search && Object.keys(item.search).length === 0)).toBe(true);
  });

  it("preserves compatible sections and clears detail search when switching scope", () => {
    expect(
      getDashboardDestination(
        { kind: "all", organizationSlug: "acme" },
        "logs",
      ),
    ).toEqual({
      to: "/cloud/$organizationSlug/~",
      params: { organizationSlug: "acme" },
      search: {},
    });

    expect(
      getDashboardDestination(
        { kind: "all", organizationSlug: "acme" },
        "environment-settings",
      ),
    ).toEqual({
      to: "/cloud/$organizationSlug/~",
      params: { organizationSlug: "acme" },
      search: {},
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
      search: {},
    });

    expect(
      getDashboardDestination(
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
      search: {},
    });
  });

  it("preserves organization destinations across organizations without environment params", () => {
    expect(getDashboardDestination({ kind: "all", organizationSlug: "other" }, "servers")).toEqual({
      to: "/cloud/$organizationSlug/~/servers",
      params: { organizationSlug: "other" },
      search: {},
    });
  });

  it("derives active sections from route IDs rather than URL positions", () => {
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

  it("drops Billing on Self-hosted Cloud", () => {
    const items = createDashboardNavItems({ kind: "all", organizationSlug: "acme" });
    expect(items.map((item) => item.section)).not.toContain("billing");
  });
});
