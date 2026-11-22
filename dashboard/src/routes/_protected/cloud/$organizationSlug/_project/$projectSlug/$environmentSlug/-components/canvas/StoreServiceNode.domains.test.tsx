// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { afterEach, expect, it, vi } from "vitest";
import type { DomainRow, RowId, ServiceListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import * as runtime from "#/providers/runtime-provider";
import * as uptime from "#/modules/uptime/uptime.queries";
import * as views from "#/modules/config-store/store-view.queries";
import * as removal from "../../_canvas/services/$serviceId/-components/useRemoveStoreService";
import * as trouble from "./use-container-trouble";
import { StoreServiceCard } from "./StoreServiceNode";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

const store = { project: "shop", environment: "production" };
const service = asTestDouble<ServiceListing>()({ id: "web-id", row: "web-id:node" as RowId, name: "web", private_dns: "web", source: "image", change: null, template: null });
const acme = asTestDouble<DomainRow>()({ kind: "custom", hostname: "acme.com", service: "web", status: "ready", port: null });

async function showCard(read: () => ReturnType<typeof views.useStoreView>) {
  vi.spyOn(views, "useStoreView").mockImplementation(read);
  vi.spyOn(views, "useInFlightDeployments").mockReturnValue([]);
  vi.spyOn(runtime, "useRuntimeService").mockReturnValue(asTestDouble<ReturnType<typeof runtime.useRuntimeService>>()({ runtime: null }));
  vi.spyOn(uptime, "useUnreachableHost").mockReturnValue(null);
  vi.spyOn(trouble, "useContainerTrouble").mockReturnValue(null);
  vi.spyOn(removal, "useRemoveStoreService").mockReturnValue(vi.fn());
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet,
    beforeLoad: () => ({ session: { session: { id: "session-id" }, user: { id: "user-id" } } }) });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug",
    loader: () => ({ store, environmentId: "env-id" }), component: () => (
      <StoreServiceCard service={service} changeCount={0} runtimeIdentity={null} desiredReplicas={null} replicas={1} trays={[]} configTrays={[]} notSet={[]}
        selected={false} className="block" />
    ) });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production"] }),
  });
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);
  return screen.findByRole("link", { name: /^web/ });
}

it("keeps a Service's open menu when its domains land", async () => {
  let land = () => {};
  const domains = new Promise<void>((resolve) => { land = resolve; });
  let landed = false;
  const card = await showCard(() => {
    // As `useSuspenseQuery` does while the Cluster hasn't answered.
    if (!landed) throw domains;
    return asTestDouble<ReturnType<typeof views.useStoreView>>()({ ok: true, value: { domains: [acme] } });
  });
  fireEvent.contextMenu(card);
  expect(await screen.findByRole("menuitem", { name: "View settings" })).toBeTruthy();

  await act(async () => {
    landed = true;
    land();
    await domains;
  });

  expect(await screen.findByText("acme.com")).toBeTruthy();
  expect(screen.getByRole("menuitem", { name: "View settings" })).toBeTruthy();
  // The open menu hides the rest of the page from the accessibility tree.
  expect(screen.getByRole("link", { name: /^web, acme\.com/, hidden: true })).toBe(card);
});

it("shows a Service whose domains fail without them", async () => {
  vi.spyOn(console, "error").mockImplementation(() => {});
  const card = await showCard(() => {
    throw new Error("The Cluster didn't answer");
  });
  expect(card.textContent).toContain("web");
});
