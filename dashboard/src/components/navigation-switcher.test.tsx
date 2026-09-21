// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { getEnvironmentSummariesCollection } from "#/collections/collections";
import { organizationKeys } from "#/modules/environment-design/workspace-queries";
import { NavigationSwitcher } from "./navigation-switcher";

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Element.prototype.scrollIntoView ??= () => {};
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it.each(["desktop", "mobile", "rail"] as const)("offers the same scoped org and combined project/environment choices in %s", async (projection) => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  for (const organization of ["acme", "other"]) {
    queryClient.setQueryData(["collections", "session", "user", organization, "project_preference"],
      organization === "acme" ? [{ id: "other-project", environmentId: "other-production" }] : []);
    queryClient.setQueryData(organizationKeys.state(organization), {
      activeOrganization: { name: organization === "acme" ? "Acme" : "Other org" },
      organizations: [{ id: "acme", slug: "acme", name: "Acme" }, { id: "other", slug: "other", name: "Other org" }],
    });
    queryClient.setQueryData(["collections", "session", "user", organization, "project"], organization === "acme" ? [
      { id: "project", slug: "store", name: "Store", resolvedEnvironment: { namespace: "production", name: "Production" } },
      { id: "other-project", slug: "docs", name: "Docs", resolvedEnvironment: { namespace: "production", name: "Other production" } },
    ] : []);
    queryClient.setQueryData(["collections", "session", "user", organization, "environment_summary"], organization === "acme" ? [
      { createdAt: new Date(0), id: "production", projectId: "project", namespace: "production", name: "Production" },
      { createdAt: new Date(1), id: "staging", projectId: "project", namespace: "staging", name: "Staging" },
      { createdAt: new Date(0), id: "other-development", projectId: "other-project", namespace: "development", name: "Other development" },
      { createdAt: new Date(1), id: "other-production", projectId: "other-project", namespace: "production", name: "Other production" },
    ] : []);
  }
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }), component: Outlet });
  const organizationRoute = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: () => <NavigationSwitcher projection={projection} /> });
  const projectGroup = createRoute({ getParentRoute: () => organizationRoute, id: "_project" });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug" });
  const logs = createRoute({ getParentRoute: () => environment, path: "logs" });
  const organizationGroup = createRoute({ getParentRoute: () => organizationRoute, id: "_org" });
  const organizationHome = createRoute({ getParentRoute: () => organizationGroup, path: "~" });
  const routeTree = root.addChildren([protectedRoute.addChildren([organizationRoute.addChildren([projectGroup.addChildren([environment.addChildren([logs])]), organizationGroup.addChildren([organizationHome])])])]);
  const router = createRouter({ routeTree, history: createMemoryHistory({ initialEntries: ["/cloud/acme/store/production/logs?serviceId=old&tab=networking"] }) });
  try {
    render(<><input aria-label="Current editor" /><QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider></>);
    const projectTrigger = await screen.findByRole("button", { name: "Project and environment: Store / Production" });
    if (projection === "rail") {
      screen.getByRole("textbox", { name: "Current editor" }).focus();
      fireEvent.mouseEnter(projectTrigger);
      fireEvent.mouseMove(projectTrigger);
    }
    else fireEvent.click(projectTrigger);
    expect(await screen.findByRole("option", { name: "Staging" })).toBeTruthy();
    expect(screen.queryByRole("option", { name: "Other production" })).toBeNull();
    fireEvent.pointerMove(screen.getByRole("option", { name: "Docs" }));
    expect(await screen.findByRole("option", { name: "Other production" })).toBeTruthy();
    await waitFor(() => expect(screen.getByRole("option", { name: "Other production" }).getAttribute("aria-selected")).toBe("true"));
    expect(screen.getByRole("option", { name: "Other development" }).getAttribute("aria-selected")).toBe("false");
    expect(screen.getByRole("option", { name: "Store" }).getAttribute("data-checked")).toBe("true");
    expect(router.state.location.pathname).toBe("/cloud/acme/store/production/logs");
    expect(screen.getByRole("link", { name: "New project" }).getAttribute("href")).toBe("/cloud/acme/new");
    expect(screen.getByRole("button", { name: "New environment" })).toBeTruthy();
    fireEvent.pointerMove(screen.getByRole("option", { name: "Store" }));
    fireEvent.click(await screen.findByRole("option", { name: "Staging" }));
    await waitFor(() => expect(router.state.location.href).toBe("/cloud/acme/store/staging/logs"));
    fireEvent.click(screen.getByRole("button", { name: "Project and environment: Store / Staging" }));
    fireEvent.click(await screen.findByRole("option", { name: "Docs" }));
    await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/docs/production/logs"));
    const organizationTrigger = screen.getByRole("button", { name: "Organization: Acme" });
    if (projection === "rail") {
      fireEvent.mouseEnter(organizationTrigger);
      fireEvent.mouseMove(organizationTrigger);
    }
    else fireEvent.click(organizationTrigger);
    fireEvent.click(await screen.findByRole("option", { name: "Other org" }));
    await waitFor(() => expect(router.state.location.href).toBe("/cloud/other/~"));
  } finally {
    cleanup();
    await Promise.all([getEnvironmentSummariesCollection("acme", scope).cleanup(), getEnvironmentSummariesCollection("other", scope).cleanup()]);
    queryClient.clear();
  }
});
