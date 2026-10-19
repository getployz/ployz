// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, waitFor } from "@testing-library/react";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
import { StoreEnvironmentSettings, StoreProjectSettings } from "./store-settings";
import { asTestDouble } from "#/lib/test-double";
import * as storeViews from "#/modules/config-store/store-view.queries";

// The Store deletes a Project in one piece once its last removal applied: its Environments read as not found.
vi.spyOn(storeViews, "useStoreView").mockReturnValue(asTestDouble<ReturnType<typeof storeViews.useStoreView>>()({
  ok: false, refusal: { code: "not_found", message: "No Project named shop", details: {} },
}));

afterEach(() => { cleanup(); });

function open(Settings: typeof StoreProjectSettings) {
  const root = createRootRoute({ component: Outlet });
  const signedIn = createRoute({
    getParentRoute: () => root, id: "_protected", component: Outlet,
    beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }),
  });
  const projects = createRoute({ getParentRoute: () => signedIn, path: "cloud/$organizationSlug/~", component: () => <p>Projects</p> });
  const settings = createRoute({
    getParentRoute: () => signedIn, path: "cloud/$organizationSlug/$projectSlug/$environmentSlug/settings",
    component: () => <Settings organizationSlug="acme" projectSlug="shop" environmentSlug="production" />,
  });
  const router = createRouter({
    routeTree: root.addChildren([signedIn.addChildren([projects, settings])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production/settings"] }),
  });
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);
  return router;
}

it.each([["project", StoreProjectSettings], ["environment", StoreEnvironmentSettings]])(
  "leaves the %s settings of a deleted Project for Projects", async (_, Settings) => {
    const router = open(Settings);
    await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/~"));
  });
