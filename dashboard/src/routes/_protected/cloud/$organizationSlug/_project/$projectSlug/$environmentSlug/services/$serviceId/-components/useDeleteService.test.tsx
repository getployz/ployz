// @vitest-environment jsdom
import { QueryClient, queryOptions } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import type { ServiceListing } from "@ployz/sdk";
import { expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as writes from "#/modules/config-store/store-write";
import * as views from "#/modules/config-store/store-view.queries";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { useRemoveStoreService } from "./useDeleteService";

const environment = { project: "shop", environment: "production" };
const service: ServiceListing = { source: "image", change: null, template: { id: "postgres", version: 1 }, id: "db", name: "db", private_dns: "db" };

function Remove() {
  return <button type="button" onClick={useRemoveStoreService(environment, service)}>Remove</button>;
}

it.each([
  ["accepted", true, ["remove_service", "remove_volume"]],
  ["refused", false, ["remove_service"]],
])("removes a preset's Volume, read on a cold cache, only once the Service's removal is %s", async (_, accepted, sent) => {
  // Cold cache: the Volumes view comes from a read, not from a view some component already holds.
  vi.spyOn(views, "storeViewOptions").mockReturnValue(queryOptions({ queryKey: ["volumes"], queryFn: async () => ({ ok: true,
    value: { view: "volumes", volumes: [{ name: "db-data", mounts: [{ service: "db" }] }, { name: "shared", mounts: [{ service: "db" }, { service: "web" }] }] } }) }) as never);
  const commands: { command: string }[] = [];
  const commit = vi.fn((command: { command: string }) => {
    commands.push(command);
    const promise = command.command === "remove_service" && !accepted
      ? Promise.reject(new StoreRefused({ code: "invalid_argument", message: "no", details: null })) : Promise.resolve({});
    return { isPersisted: { promise } };
  });
  vi.spyOn(writes, "useStoreWriter").mockReturnValue({ commit } as never);
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient: new QueryClient(), sessionId: "s", userId: "u" });
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const cloud = createRoute({ getParentRoute: () => protectedRoute, path: "cloud", component: Outlet });
  const organization = createRoute({ getParentRoute: () => cloud, path: "$organizationSlug", component: Outlet });
  const project = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const projectSlug = createRoute({ getParentRoute: () => project, path: "$projectSlug", component: Outlet });
  const env = createRoute({ getParentRoute: () => projectSlug, path: "$environmentSlug", component: Remove });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([cloud.addChildren([
    organization.addChildren([project.addChildren([projectSlug.addChildren([env])])]),
  ])])]), history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production"] }) });
  try {
    await router.load();
    render(<RouterProvider router={router} />);
    fireEvent.click(await screen.findByText("Remove"));
    await vi.waitFor(() => expect(commands.map((command) => command.command)).toEqual(sent));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(commands.slice(1)).toEqual(accepted ? [{ command: "remove_volume", environment, volume: "db-data" }] : []);
  } finally {
    cleanup();
    router.history.destroy();
    vi.restoreAllMocks();
  }
});
