// @vitest-environment jsdom

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
import { ProjectNameRow, StoreEnvironmentSettings, StoreProjectSettings } from "./store-settings";
import { asTestDouble } from "#/lib/test-double";
import * as storeViews from "#/modules/config-store/store-view.queries";

function refused(code: string) {
  return vi.spyOn(storeViews, "useStoreView").mockReturnValue(asTestDouble<ReturnType<typeof storeViews.useStoreView>>()({
    ok: false, refusal: { code, message: "refused", details: {} },
  }));
}

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

const pages = [["project", StoreProjectSettings], ["environment", StoreEnvironmentSettings]] as const;

// The Store deletes a Project in one piece once its last removal applied: its Environments read as not found.
it.each(pages)("leaves the %s settings of a deleted Project for Projects", async (_, Settings) => {
  refused("not_found");
  const router = open(Settings);
  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/~"));
});

it.each(pages)("keeps the %s settings open when the Store fails to read a Project that exists", async (_, Settings) => {
  const view = refused("internal");
  const router = open(Settings);
  await waitFor(() => expect(view).toHaveBeenCalled());
  await waitFor(() => expect(router.state.status).toBe("idle"));
  expect(router.state.location.pathname).toBe("/cloud/acme/shop/production/settings");
});

it("holds the project name while a rename saves", () => {
  const onRename = vi.fn(() => new Promise<void>(() => {}));
  render(<ProjectNameRow project="shop" onRename={onRename} />);
  const input = screen.getByRole("textbox", { name: "Name" });
  fireEvent.change(input, { target: { value: "store" } });
  fireEvent.click(screen.getByRole("button", { name: "Confirm" }));

  expect(onRename).toHaveBeenCalledExactlyOnceWith("store");
  expect(input.hasAttribute("disabled")).toBe(true);
  expect(screen.getByLabelText("Saving")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Confirm" })).toBeNull();
  fireEvent.keyDown(input, { key: "Enter" });
  expect(onRename).toHaveBeenCalledTimes(1);
});
