// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { organizationKeys } from "#/modules/environment-design/workspace.queries";
import { OrganizationSwitcher } from "./organization-switcher";

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Element.prototype.scrollIntoView ??= () => {};
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it.each(["desktop", "mobile", "rail"] as const)("switches organization in %s", async (projection) => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  for (const organization of ["acme", "other"]) {
    queryClient.setQueryData(organizationKeys.state(organization), {
      activeOrganization: { name: organization === "acme" ? "Acme" : "Other org" },
      organizations: [{ id: "acme", slug: "acme", name: "Acme" }, { id: "other", slug: "other", name: "Other org" }],
    });
  }
  const root = createRootRoute({ component: Outlet });
  const organizationRoute = createRoute({ getParentRoute: () => root, path: "cloud/$organizationSlug", component: () => <OrganizationSwitcher projection={projection} /> });
  const organizationGroup = createRoute({ getParentRoute: () => organizationRoute, id: "_org" });
  const organizationHome = createRoute({ getParentRoute: () => organizationGroup, path: "~" });
  const routeTree = root.addChildren([organizationRoute.addChildren([organizationGroup.addChildren([organizationHome])])]);
  const router = createRouter({ routeTree, history: createMemoryHistory({ initialEntries: ["/cloud/acme/~"] }) });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  const trigger = await screen.findByRole("button", { name: "Organization: Acme" });
  if (projection === "rail") {
    fireEvent.mouseEnter(trigger);
    fireEvent.mouseMove(trigger);
  }
  else fireEvent.click(trigger);
  fireEvent.click(await screen.findByRole("option", { name: "Other org" }));
  await waitFor(() => expect(router.state.location.href).toBe("/cloud/other/~"));
  queryClient.clear();
});
