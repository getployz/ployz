// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { EditorView } from "@codemirror/view";
import type { ConfigItemView, ConfigQuery, ConfigView, ConfigWritten, ServiceListing } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import * as functions from "#/modules/config-store/store.functions";
import { configQuery, configsQuery, diffQuery, environmentSettingsQuery, servicesQuery, storeViewOptions } from "#/modules/config-store/store-view.queries";
import type { StoreResult } from "#/modules/config-store/store.contract";
import { InspectorPresentation } from "../../../-components/CanvasInspectorHeader";
import * as changes from "../../../-components/canvas/useStoreChangeActions";
import { StoreConfigDrawer } from "./StoreConfigDrawer";

const environment = { project: "shop", environment: "production" };
const env = { id: "env", ...environment, name: "production", revision: 1 };
const params = { organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production", resourceId: "config" };
const initial: ConfigItemView = {
  environment: env, id: "config", lineage: "config", name: "sentry", deployed: false, change: "create", mounts: [],
  files: [{ name: "config.yml", bytes: 7, mode: "0444", uid: 0, gid: 0, references: [] }], contents: { "config.yml": "initial" },
};
const accepted: StoreResult<ConfigWritten> = { ok: true, value: { written: "batch", results: [] } };
const refused: StoreResult<ConfigWritten> = { ok: false, refusal: { code: "invalid", message: "Unknown Config reference", details: null } };

function deferred() {
  let resolve!: (value: StoreResult<ConfigWritten>) => void;
  const promise = new Promise<StoreResult<ConfigWritten>>((yes) => { resolve = yes; });
  return { promise, resolve };
}

afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

async function openDrawer() {
  vi.stubGlobal("scrollTo", () => {});
  vi.spyOn(changes, "useStoreChangeActions").mockReturnValue(asTestDouble<ReturnType<typeof changes.useStoreChangeActions>>()({ dialog: null }));
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "s", userId: "u" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  const store = { item: initial };
  function read(query: ConfigQuery): ConfigView {
    switch (query.query) {
      case "config": return { view: "config", ...store.item };
      case "configs": return { view: "configs", environment: env, configs: [store.item] };
      case "services": return { view: "services", environment: env, services: [asTestDouble<ServiceListing>()({ id: "api", name: "api", private_dns: "api", change: null })] };
      case "environment": return { view: "environment", environment: env, settings: [] };
      case "diff": return { view: "diff", environment: env, version: "1", saved: 0, published: false, total_count: 0,
        changes: [], hints: [], incoming: [], follow_hints: [] };
      default: throw new Error(`Unexpected test query ${query.query}`);
    }
  }
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(async ({ data }) => ({ ok: true, value: read(data.query) }));
  const write = vi.spyOn(functions, "writeStoreServerFn");
  for (const query of [configQuery(environment, "sentry"), configsQuery(environment), servicesQuery(environment), diffQuery(environment), environmentSettingsQuery(environment)]) {
    queryClient.setQueryData<unknown>(storeViewOptions("acme", scope, query).queryKey, { ok: true, value: read(query) });
  }
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const project = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environmentRoute = createRoute({ getParentRoute: () => project, path: "$projectSlug/$environmentSlug", loader: () => ({ store: environment }), component: Outlet });
  const resource = createRoute({ getParentRoute: () => environmentRoute, path: "resources/$resourceId", component: () => (
    <InspectorPresentation value={{ takeover: false, toggleFullscreen() {}, returnTo: null }}>
      <StoreConfigDrawer params={params} config={initial} />
    </InspectorPresentation>
  ) });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([
    project.addChildren([environmentRoute.addChildren([resource])]),
  ])])]), history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production/resources/config"] }) });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByRole("textbox", { name: "config.yml contents" });
  function editor() {
    const view = EditorView.findFromDOM(screen.getByRole("textbox", { name: /contents$/ }));
    if (!view) throw new Error("Config editor did not mount");
    return view;
  }
  return {
    store, write,
    edit(text: string) { act(() => { const view = editor(); view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text }, userEvent: "input.type" }); }); },
    text: () => editor().state.doc.toString(),
  };
}

it("keeps text typed during a pending save when that save commits", async () => {
  const test = await openDrawer();
  const pending = deferred();
  test.write.mockImplementationOnce(() => pending.promise);
  test.edit("submitted");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  test.edit("newer unsaved text");
  test.store.item = { ...initial, contents: { "config.yml": "submitted" } };
  await act(async () => { pending.resolve(accepted); });
  await waitFor(() => expect(test.text()).toBe("newer unsaved text"));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(false);
});

it("restores refused text and lets the user retry it", async () => {
  const test = await openDrawer();
  test.write.mockResolvedValueOnce(refused);
  test.edit("keep this text");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await screen.findByText("Unknown Config reference");
  expect(test.text()).toBe("keep this text");
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(false);
});

it("does not restore an older refused save over a newer submitted save", async () => {
  const test = await openDrawer();
  const first = deferred();
  const second = deferred();
  test.write.mockImplementationOnce(() => first.promise).mockImplementationOnce(() => second.promise);
  test.edit("older");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  test.edit("newer");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await act(async () => { first.resolve(refused); });
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(2));
  test.store.item = { ...initial, contents: { "config.yml": "newer" } };
  await act(async () => { second.resolve(accepted); });
  await waitFor(() => expect(test.text()).toBe("newer"));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true);
});

it("adds a nested filename and submits its contents", async () => {
  const test = await openDrawer();
  test.write.mockImplementationOnce(() => new Promise(() => {}));
  fireEvent.click(screen.getByRole("button", { name: "Add file" }));
  fireEvent.change(screen.getByRole("textbox", { name: "File name" }), { target: { value: "conf.d/site.yml" } });
  fireEvent.click(screen.getByRole("button", { name: "Add" }));
  await screen.findByRole("textbox", { name: "conf.d/site.yml contents" });
  test.edit("listen: 8080");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(test.write).toHaveBeenCalledWith(expect.objectContaining({ data: {
    organizationSlug: "acme", command: { command: "batch", environment, commands: [
      { command: "put_config_file", environment, config: "sentry", file: "conf.d/site.yml", content: "listen: 8080" },
    ] },
  } })));
});


it("restores a refused mount form without losing its directory", async () => {
  const test = await openDrawer();
  test.write.mockResolvedValueOnce(refused);
  fireEvent.click(screen.getByRole("combobox", { name: "Service" }));
  fireEvent.click(await screen.findByRole("option", { name: "api" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Directory" }), { target: { value: "/etc/attempt" } });
  fireEvent.click(screen.getByRole("button", { name: "Mount" }));
  await screen.findByText("Unknown Config reference");
  expect(screen.getByRole<HTMLInputElement>("textbox", { name: "Directory" }).value).toBe("/etc/attempt");
  expect(screen.getByRole("combobox", { name: "Service" }).textContent).toContain("api");
});
