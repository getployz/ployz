// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { Schema } from "effect";
import type { ConfigQuery, ConfigView, MachineId, ServiceListing, VolumeListing } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import * as mounts from "#/modules/config-store/StoreVolumeMounts";
import * as functions from "#/modules/config-store/store.functions";
import { diffQuery, environmentSettingsQuery, servicesQuery, storeViewOptions, volumesQuery } from "#/modules/config-store/store-view.queries";
import { InspectorPresentation } from "../../../-components/CanvasInspectorHeader";
import * as changes from "../../../-components/canvas/useStoreChangeActions";
import * as copies from "./StoreVolumeCopies";
import { StoreVolumeDrawer } from "./StoreVolumeDrawer";
import { volumeSearchSchema } from "./volume-pages";

const environment = { project: "shop", environment: "production" };
const env = { id: "env", ...environment, name: "production", revision: 1 };

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

async function openDrawer(url: string, volume: Partial<VolumeListing>, copy: MachineId | null = null) {
  vi.stubGlobal("scrollTo", () => {});
  vi.spyOn(changes, "useStoreChangeActions").mockReturnValue(asTestDouble<ReturnType<typeof changes.useStoreChangeActions>>()({ dialog: null }));
  vi.spyOn(copies, "useVolumeCopies").mockReturnValue(asTestDouble<ReturnType<typeof copies.useVolumeCopies>>()({ copies: null }));
  vi.spyOn(mounts, "StoreVolumeMounts").mockImplementation(() => <></>);
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "s", userId: "u" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  const listing: VolumeListing = {
    id: "vol", name: "data", mounts: [], deployed: false, storage_locked: false, change: null,
    storage: { kind: "docker" }, shared_writes: false, ...volume,
  };
  const services = [...new Set(listing.mounts.map((mount) => mount.service))]
    .map((name) => asTestDouble<ServiceListing>()({ id: name, name, private_dns: name, change: null }));
  function read(query: ConfigQuery): ConfigView {
    switch (query.query) {
      case "volumes": return { view: "volumes", environment: env, volumes: [listing] };
      case "services": return { view: "services", environment: env, services };
      case "environment": return { view: "environment", environment: env, settings: [] };
      case "diff": return { view: "diff", environment: env, version: "1", saved: 0, holds_staged: false, total_count: 0,
        changes: [], incoming: [], follow_hints: [], included: [] };
      default: throw new Error(`Unexpected test query ${query.query}`);
    }
  }
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(async ({ data }) => ({ ok: true, value: read(data.query) }));
  for (const query of [volumesQuery(environment), servicesQuery(environment), diffQuery(environment), environmentSettingsQuery(environment)]) {
    queryClient.setQueryData<unknown>(storeViewOptions("acme", scope, query).queryKey, { ok: true, value: read(query) });
  }
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const project = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environmentRoute = createRoute({ getParentRoute: () => project, path: "$projectSlug/$environmentSlug", loader: () => ({ store: environment }), component: Outlet });
  const canvas = createRoute({ getParentRoute: () => environmentRoute, id: "_canvas", component: Outlet });
  const resource = createRoute({
    getParentRoute: () => canvas, path: "resources/$resourceId", validateSearch: Schema.toStandardSchemaV1(volumeSearchSchema),
    component: () => (
      <InspectorPresentation value={{ takeover: false, toggleFullscreen() {} }}>
        <StoreVolumeDrawer params={{ organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production", resourceId: "vol" }} copy={copy} />
      </InspectorPresentation>
    ),
  });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([
    project.addChildren([environmentRoute.addChildren([canvas.addChildren([resource])])]),
  ])])]), history: createMemoryHistory({ initialEntries: [url] }) });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByRole("tab", { name: "Copies" });
}

it("opens a mirror on the Copies tab, even from a URL that had Settings open", async () => {
  await openDrawer("/cloud/acme/shop/production/resources/vol~web-2?tab=settings", {}, "web-2" as MachineId);
  expect(screen.getByRole("tab", { name: "Copies" }).getAttribute("aria-selected")).toBe("true");
});
