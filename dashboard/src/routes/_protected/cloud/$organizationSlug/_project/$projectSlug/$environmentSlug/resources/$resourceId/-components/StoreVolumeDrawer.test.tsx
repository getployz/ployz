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
import * as runs from "#/modules/volume-run/volume-run.functions";
import type { Holder } from "#/modules/volume-run/resize-check";
import type { VolumeRunView } from "#/modules/volume-run/volume-run";
import { run } from "#/modules/volume-run/volume-run.test-fixture";

const environment = { project: "shop", environment: "production" };
const env = { id: "env", ...environment, name: "production", revision: 1 };

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

type CopiesData = ReturnType<typeof copies.useVolumeCopies>;

async function openDrawer(url: string, volume: Partial<VolumeListing>, copy: MachineId | null = null, { realMounts = false, data = {} }: { realMounts?: boolean; data?: Partial<CopiesData> } = {}) {
  vi.stubGlobal("scrollTo", () => {});
  vi.spyOn(changes, "useStoreChangeActions").mockReturnValue(asTestDouble<ReturnType<typeof changes.useStoreChangeActions>>()({ dialog: null }));
  vi.spyOn(copies, "useVolumeCopies").mockReturnValue(asTestDouble<CopiesData>()({ copies: null, holders: null, running: null, ...data }));
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
  return queryClient;
}

it("opens a mirror on the Copies tab, even from a URL that had Settings open", async () => {
  await openDrawer("/cloud/acme/shop/production/resources/vol~web-2?tab=settings", {}, "web-2" as MachineId);
  expect(screen.getByRole("tab", { name: "Copies" }).getAttribute("aria-selected")).toBe("true");
});

it("locks the drawer and shows the new name while its rename is pending, so no edit names the volume by its old name", async () => {
  await openDrawer("/cloud/acme/shop/production/resources/vol?tab=settings", {});
  let answer!: (result: StoreResult<ConfigWritten>) => void;
  const write = vi.spyOn(functions, "writeStoreServerFn").mockImplementationOnce(() => new Promise((resolve) => { answer = resolve; }));
  fireEvent.click(screen.getByTitle("Edit volume name"));
  fireEvent.change(screen.getByRole("textbox", { name: "Edit volume name" }), { target: { value: "store" } });
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Save" }));
  await waitFor(() => expect(write).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(screen.getByRole("tab", { name: "Settings", hidden: true }).closest("[inert]")).not.toBeNull());
  expect(screen.getByTitle("Edit volume name").closest("[inert]")).not.toBeNull();
  expect(screen.getByTitle("Edit volume name").textContent).toBe("store");
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
  ["the Docker volume switch", "switch", "Use a plain Docker volume (not recommended)", "false"],
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

const GB = 1_000_000_000;
const SIZE = ["ployz.volume.size.set.v1"];
const holders = (fsn2: readonly string[] = SIZE): Holder[] => [
  { name: "fsn-1", role: "primary", capabilities: SIZE, committableBytes: 40 * GB },
  { name: "fsn-2", role: "mirror", capabilities: fsn2, committableBytes: null },
];
const deployed = (maximumBytes = 10 * GB): Partial<VolumeListing> =>
  ({ deployed: true, storage_locked: true, storage: { kind: "provisioned", maximumBytes }, recorded_storage: 10 * GB });
const SETTINGS = "/cloud/acme/shop/production/resources/vol?tab=settings";
const limit = () => screen.getByRole("textbox", { name: "Storage limit" });
const enter = (value: string) => {
  fireEvent.change(limit(), { target: { value } });
  fireEvent.keyDown(limit(), { key: "Enter" });
};

it("grows a deployed volume after a plain confirm, then shows the run until it ends", async () => {
  await openDrawer(SETTINGS, deployed(), null, { data: { holders: holders() } });
  expect((limit() as HTMLInputElement).value).toBe("10");
  screen.getByText("Grows live on every server that holds a copy. Can't shrink yet.");
  const resize = vi.spyOn(runs, "resizeVolumeServerFn").mockResolvedValue({ id: "run-1" });
  const growing: VolumeRunView = { ...run("resize", { maximum_bytes: 20 * GB }, "running"), id: "run-1", message: "Growing data to 20 GB on fsn-2" };
  const read = vi.spyOn(runs, "getVolumeRunServerFn").mockResolvedValue(growing);
  enter("20");
  const dialog = await screen.findByRole("alertdialog");
  within(dialog).getByText("Grow data from 10 GB to 20 GB on fsn-1 and its mirror on fsn-2. Its services keep running.");
  fireEvent.click(within(dialog).getByRole("button", { name: "Grow" }));
  await waitFor(() => expect(resize).toHaveBeenCalledTimes(1));
  expect(resize.mock.calls[0]?.[0]?.data).toMatchObject({ organizationSlug: "acme", volumeId: "vol", environment, maximumBytes: 20 * GB });
  await screen.findByText("Growing data to 20 GB on fsn-2");
  expect(limit().hasAttribute("disabled")).toBe(true);
  read.mockResolvedValue({ ...growing, state: "failed", message: "fsn-2 has no room to grow data to 20 GB" });
  await screen.findByText("fsn-2 has no room to grow data to 20 GB", {}, { timeout: 5_000 });
  expect(limit().hasAttribute("disabled")).toBe(false);
});

it.each([
  ["a smaller size, as the Store would", holders(), "5", "data is deployed at 10 GB; a deployed volume can't shrink yet."],
  ["a size a holder's Ployz can't set", holders(["ployz.volume.copy.set.v1"]), "20", "Growing needs a newer Ployz on fsn-2."],
  ["more than the tightest Pool has room for", holders(), "60", "fsn-1 has room to grow it by 40 GB at most."],
])("refuses %s in the field, before any confirm", async (_, held, typed, message) => {
  await openDrawer(SETTINGS, deployed(), null, { data: { holders: held } });
  enter(typed);
  await screen.findByText(message);
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

it("shows a resize the Servers refuse at admission in the field", async () => {
  await openDrawer(SETTINGS, deployed(), null, { data: { holders: holders() } });
  vi.spyOn(runs, "resizeVolumeServerFn").mockRejectedValue(new Error("data has a move in progress"));
  enter("20");
  fireEvent.click(within(await screen.findByRole("alertdialog")).getByRole("button", { name: "Grow" }));
  await screen.findByText("data has a move in progress");
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

it("stages the held size back over a restored smaller revision, with no resize", async () => {
  await openDrawer(SETTINGS, deployed(4 * GB), null, { data: { holders: holders() } });
  screen.getByText("A restored revision asks for 4 GB; the servers keep 10 GB. Enter 10 to keep it.");
  expect((limit() as HTMLInputElement).value).toBe("4");
  const write = vi.spyOn(functions, "writeStoreServerFn").mockResolvedValue({ ok: true, value: { written: "batch", results: [] } });
  const resize = vi.spyOn(runs, "resizeVolumeServerFn");
  enter("10");
  await waitFor(() => expect(write).toHaveBeenCalledTimes(1));
  expect(JSON.stringify(write.mock.calls[0]?.[0])).toContain('"maximumBytes":10000000000');
  expect(resize).not.toHaveBeenCalled();
});

it("waits for another run on the volume before it grows", async () => {
  await openDrawer(SETTINGS, deployed(), null, { data: { holders: holders(), running: run("move", { to: "fsn-2" }, "running") } });
  screen.getByText("Moving to fsn-2. Growing waits for it to finish.");
  expect(limit().hasAttribute("disabled")).toBe(true);
});

it("keeps a deployed Docker volume fixed", async () => {
  await openDrawer(SETTINGS, { deployed: true, storage_locked: true, storage: { kind: "docker" } });
  screen.getByText("Fixed after the first deploy.");
  expect(screen.queryByRole("textbox", { name: "Storage limit" })).toBeNull();
});
