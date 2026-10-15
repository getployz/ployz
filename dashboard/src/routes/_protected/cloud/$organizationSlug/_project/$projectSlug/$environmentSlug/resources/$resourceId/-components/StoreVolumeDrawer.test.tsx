// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, renderHook, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import type { ReactNode } from "react";
import { Schema } from "effect";
import type { ConfigQuery, ConfigView, ConfigWritten, MachineId, ServiceListing, VolumeListing } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import * as mounts from "#/modules/config-store/StoreVolumeMounts";
import * as functions from "#/modules/config-store/store.functions";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { diffQuery, environmentSettingsQuery, servicesQuery, storeViewOptions, volumesQuery } from "#/modules/config-store/store-view.queries";
import { useStoreWriter } from "#/modules/config-store/store-write";
import { StoreRenameLock } from "../../../-components/StoreRenameLock";
import { InspectorPresentation } from "../../../-components/CanvasInspectorHeader";
import * as changes from "../../../-components/canvas/useStoreChangeActions";
import * as copies from "./StoreVolumeCopies";
import { StoreVolumeDrawer } from "./StoreVolumeDrawer";
import { volumeSearchSchema } from "./volume-pages";

const environment = { project: "shop", environment: "production" };
const env = { id: "env", ...environment, name: "production", revision: 1 };

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

async function openDrawer(url: string, volume: Partial<VolumeListing>, copy: MachineId | null = null, { realMounts = false } = {}) {
  vi.stubGlobal("scrollTo", () => {});
  vi.spyOn(changes, "useStoreChangeActions").mockReturnValue(asTestDouble<ReturnType<typeof changes.useStoreChangeActions>>()({ dialog: null }));
  vi.spyOn(copies, "useVolumeCopies").mockReturnValue(asTestDouble<ReturnType<typeof copies.useVolumeCopies>>()({ copies: null }));
  if (!realMounts) vi.spyOn(mounts, "StoreVolumeMounts").mockImplementation(() => <></>);
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
  const canvas = createRoute({ getParentRoute: () => environmentRoute, id: "_canvas", component: () => <StoreRenameLock><Outlet /></StoreRenameLock> });
  const resource = createRoute({
    getParentRoute: () => canvas, path: "resources/$resourceId", validateSearch: Schema.toStandardSchemaV1(volumeSearchSchema),
    component: () => (
      <InspectorPresentation value={{ takeover: false, toggleFullscreen() {}, returnTo: null }}>
        <StoreVolumeDrawer params={{ organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production", resourceId: "vol" }} copy={copy} />
      </InspectorPresentation>
    ),
  });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([
    project.addChildren([environmentRoute.addChildren([canvas.addChildren([resource])])]),
  ])])]), history: createMemoryHistory({ initialEntries: [url] }) });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByRole("tab", { name: "Copies" });
  return queryClient;
}

it("opens a mirror on the Copies tab, even from a URL that had Settings open", async () => {
  await openDrawer("/cloud/acme/shop/production/resources/vol~web-2?tab=settings", {}, "web-2" as MachineId);
  expect(screen.getByRole("tab", { name: "Copies" }).getAttribute("aria-selected")).toBe("true");
});

it("locks the drawer while its rename is pending, so no edit names the volume by its old name", async () => {
  await openDrawer("/cloud/acme/shop/production/resources/vol?tab=settings", {});
  let answer!: (result: StoreResult<ConfigWritten>) => void;
  const write = vi.spyOn(functions, "writeStoreServerFn").mockImplementationOnce(() => new Promise((resolve) => { answer = resolve; }));
  fireEvent.click(screen.getByTitle("Edit volume name"));
  fireEvent.change(screen.getByRole("textbox", { name: "Edit volume name" }), { target: { value: "store" } });
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(write).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(screen.getByRole("tab", { name: "Settings", hidden: true }).closest("[inert]")).not.toBeNull());
  expect(screen.getByTitle("Edit volume name").closest("[inert]")).not.toBeNull();
  await act(async () => { answer({ ok: true, value: { written: "batch", results: [] } }); });
  await waitFor(() => expect(screen.getByRole("tab", { name: "Settings" }).closest("[inert]")).toBeNull());
  expect(screen.getByTitle("Edit volume name").closest("[inert]")).toBeNull();
});

it("locks a Volume's mounts while a Service they name is renaming, so Unmount never names it by its old name", async () => {
  const queryClient = await openDrawer("/cloud/acme/shop/production/resources/vol?tab=settings",
    { mounts: [asTestDouble<VolumeListing["mounts"][number]>()({ service: "web", path: "/data", change: null })] }, null, { realMounts: true });
  let answer!: (result: StoreResult<ConfigWritten>) => void;
  vi.spyOn(functions, "writeStoreServerFn").mockImplementationOnce(() => new Promise((resolve) => { answer = resolve; }));
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const writer = renderHook(() => useStoreWriter("acme"), { wrapper }).result.current;
  const renamed = writer.commit({ command: "rename_service", environment, service: "web", name: "site" }).isPersisted.promise;
  const actions = () => screen.getByRole("button", { name: "Actions for web mount", hidden: true });
  await waitFor(() => expect(actions().closest("[inert]")).not.toBeNull());
  await act(async () => { answer({ ok: true, value: { written: "batch", results: [] } }); await renamed; });
  await waitFor(() => expect(actions().closest("[inert]")).toBeNull());
});

it.each([
  ["the Docker volume checkbox", "checkbox", "Use a plain Docker volume (not recommended)", "false"],
  ["the shared writes switch", "switch", "Allow shared writes", "true"],
] as const)("holds %s at the submitted value while its write runs", async (_, role, name, submitted) => {
  await openDrawer("/cloud/acme/shop/production/resources/vol?tab=settings", {});
  let answer!: (result: StoreResult<ConfigWritten>) => void;
  const write = vi.spyOn(functions, "writeStoreServerFn").mockImplementationOnce(() => new Promise((resolve) => { answer = resolve; }));
  const control = () => screen.getByRole(role, { name });
  fireEvent.click(control());
  await waitFor(() => expect(write).toHaveBeenCalledTimes(1));
  expect(control().getAttribute("aria-checked")).toBe(submitted);
  await act(async () => { answer({ ok: true, value: { written: "batch", results: [] } }); });
  await waitFor(() => expect(control().getAttribute("aria-checked")).not.toBe(submitted));
});
