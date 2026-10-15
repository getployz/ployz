// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { MutationObserver, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { afterEach, expect, it } from "vitest";
import type { ConfigCommand, ConfigListing, VolumeListing } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { storeWriteKey } from "#/modules/config-store/store-pending";
import { CanvasNodeList } from "./CanvasServiceList";

const store = { project: "shop", environment: "production" };
const volume = asTestDouble<VolumeListing>()({ id: "vol", name: "data", mounts: [], deployed: false, storage: { kind: "docker" }, storage_locked: false, change: null });
const config = asTestDouble<ConfigListing>()({ id: "cfg", name: "nginx", change: null });

afterEach(cleanup);

async function show(pending: readonly ConfigCommand[]) {
  const queryClient = new QueryClient();
  for (const command of pending) {
    void new MutationObserver<unknown, Error, { command: ConfigCommand }>(queryClient, { mutationKey: storeWriteKey("acme", command.command), mutationFn: () => new Promise(() => {}) }).mutate({ command });
  }
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug", loader: () => ({ store }), component: () => (
    <CanvasNodeList store={{ services: [], unmountedVolumes: [volume], unmountedConfigs: [config] }} selectedNodeId={null} />
  ) });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production"] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByRole("link", { name: /nginx|settings/ });
}

const card = (id: string) => document.querySelector(`[data-canvas-node="${id}"] [data-slot="card"]`);

it("titles a Volume and a Config by their pending new names until the Store answers", async () => {
  await show([
    { command: "rename_volume", environment: store, volume: "data", name: "store" },
    { command: "rename_config", environment: store, config: "nginx", name: "settings" },
  ]);
  expect(screen.getByRole("link", { name: /store/ })).toBeTruthy();
  expect(screen.getByRole("link", { name: /settings/ })).toBeTruthy();
  expect(screen.queryByText("data")).toBeNull();
  expect(screen.queryByText("nginx")).toBeNull();
});

it("dims a Volume and a Config a pending removal is taking away, and only those", async () => {
  await show([{ command: "remove_volume", environment: store, volume: "data" }]);
  expect(card("vol")?.getAttribute("data-leaving")).toBe("true");
  expect(card("cfg")?.hasAttribute("data-leaving")).toBe(false);
  cleanup();
  await show([{ command: "delete_config", environment: store, config: "nginx" }]);
  expect(card("cfg")?.getAttribute("data-leaving")).toBe("true");
  expect(card("vol")?.hasAttribute("data-leaving")).toBe(false);
});
