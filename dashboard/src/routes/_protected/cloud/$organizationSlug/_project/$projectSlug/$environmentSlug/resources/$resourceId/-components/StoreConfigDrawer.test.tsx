// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { EditorView } from "@codemirror/view";
import { undo } from "@codemirror/commands";
import { startCompletion } from "@codemirror/autocomplete";
import type { ConfigItemView, ConfigQuery, ConfigView, ConfigWritten, EnvironmentView, ServiceListing } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import * as functions from "#/modules/config-store/store.functions";
import { configQuery, configsQuery, diffQuery, environmentSettingsQuery, requireView, servicesQuery, storeViewOptions, useStoreViews } from "#/modules/config-store/store-view.queries";
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

async function openDrawer(item = initial, settings: EnvironmentView["settings"] = []) {
  vi.stubGlobal("scrollTo", () => {});
  vi.spyOn(changes, "useStoreChangeActions").mockReturnValue(asTestDouble<ReturnType<typeof changes.useStoreChangeActions>>()({ dialog: null }));
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "s", userId: "u" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  const store = { item };
  function read(query: ConfigQuery): ConfigView {
    switch (query.query) {
      case "config": return { view: "config", ...store.item };
      case "configs": return { view: "configs", environment: env, configs: [store.item] };
      case "services": return { view: "services", environment: env, services: [asTestDouble<ServiceListing>()({ id: "api", name: "api", private_dns: "api", change: null })] };
      case "environment": return { view: "environment", environment: env, settings };
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
  const resource = createRoute({ getParentRoute: () => environmentRoute, path: "resources/$resourceId", component: () => {
    const config = requireView(useStoreViews("acme", [configsQuery(environment)] as const)[0]).configs[0];
    if (!config) throw new Error("Config listing did not load");
    return (
      <InspectorPresentation value={{ takeover: false, toggleFullscreen() {}, returnTo: null }}>
        <StoreConfigDrawer params={params} config={config} />
      </InspectorPresentation>
    );
  } });
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
    store, write, router,
    edit(text: string) { act(() => { const view = editor(); view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: text }, userEvent: "input.type" }); }); },
    complete() { act(() => { const view = editor(); view.dispatch({ selection: { anchor: view.state.doc.length } }); startCompletion(view); }); },
    undo() { act(() => { undo(editor()); }); },
    text: () => editor().state.doc.toString(),
  };
}

it("recognizes unexported Config references without offering them in autocomplete", async () => {
  const test = await openDrawer(initial, [
    { path: "api.env.CONFIG_SENTINEL", value: { secret: true }, default: null, apply: "staged" },
    { path: "api.env.CONFIG_SENTINEL.exported", value: false, default: false, apply: "staged" },
    { path: "api.env.CONFIG_MODE", value: "debug", default: null, apply: "staged" },
    { path: "api.env.CONFIG_MODE.exported", value: false, default: false, apply: "staged" },
  ]);
  test.edit("token: ${{ api.CONFIG_SENTINEL }}\nmode: ${{ api.CONFIG_MODE }}\nmissing: ${{ api.MISSING }}");
  expect(screen.getByRole("region", { name: "Depends on" }).textContent).toContain("api");
  expect(within(screen.getByRole("region", { name: "Secrets" })).getByText("api.CONFIG_SENTINEL")).toBeTruthy();
  const editor = screen.getByRole("textbox", { name: "config.yml contents" });
  expect(Array.from(editor.querySelectorAll(".cm-config-ref"), (token) => token.textContent))
    .toEqual(["${{ api.CONFIG_SENTINEL }}", "${{ api.CONFIG_MODE }}"]);
  expect(Array.from(editor.querySelectorAll(".cm-config-unknown"), (token) => token.textContent))
    .toEqual(["${{ api.MISSING }}"]);
  fireEvent.click(screen.getByRole("button", { name: "Preview" }));
  const preview = screen.getByLabelText("Preview");
  expect(within(preview).getByLabelText("Secret")).toBeTruthy();
  expect(preview.textContent).toContain("mode: debug");
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  await screen.findByRole("textbox", { name: "config.yml contents" });
  test.edit("${{ api.");
  test.complete();
  await screen.findByRole("option", { name: /api.PORT/ });
  expect(screen.queryByRole("option", { name: /CONFIG_SENTINEL|CONFIG_MODE/ })).toBeNull();
});

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
  const read = vi.mocked(functions.readStoreViewServerFn).getMockImplementation();
  if (!read) throw new Error("Store test reader did not mount");
  vi.mocked(functions.readStoreViewServerFn).mockRejectedValue(new TypeError("Failed to fetch"));
  await act(async () => { first.resolve(refused); });
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(2));
  expect(test.text()).toBe("newer");
  expect(screen.queryByText("Unknown Config reference")).toBeNull();
  act(() => { void test.router.navigate({ to: "/" }); });
  const dialog = await screen.findByRole("alertdialog");
  fireEvent.click(within(dialog).getByRole("button", { name: "Keep editing" }));
  vi.mocked(functions.readStoreViewServerFn).mockImplementation(read);
  test.store.item = { ...initial, contents: { "config.yml": "newer" } };
  await act(async () => { second.resolve(accepted); });
  await waitFor(() => expect(test.text()).toBe("newer"));
  expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true);
  await act(async () => { await test.router.navigate({ to: "/" }); });
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

it("keeps a queued save visible through the preceding save's refresh and subsequent edits", async () => {
  const test = await openDrawer();
  const first = deferred();
  const second = deferred();
  const third = deferred();
  test.write.mockImplementationOnce(() => first.promise).mockImplementationOnce(() => second.promise).mockImplementationOnce(() => third.promise);
  test.edit("version: A");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  test.edit("version: B");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true));
  test.store.item = { ...initial, contents: { "config.yml": "version: A" } };
  await act(async () => { first.resolve(accepted); });
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(2));
  expect(test.text()).toBe("version: B");
  test.edit(`${test.text()}\nadded: true`);
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  test.store.item = { ...initial, contents: { "config.yml": "version: B" } };
  await act(async () => { second.resolve(accepted); });
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(3));
  expect(test.write).toHaveBeenLastCalledWith(expect.objectContaining({ data: {
    organizationSlug: "acme", command: { command: "batch", environment, commands: [
      { command: "put_config_file", environment, config: "sentry", file: "config.yml", content: "version: B\nadded: true" },
    ] },
  } }));
  expect(test.text()).toBe("version: B\nadded: true");
  test.store.item = { ...initial, contents: { "config.yml": "version: B\nadded: true" } };
  await act(async () => { third.resolve(accepted); });
  await waitFor(() => expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true));
});

it("keeps unsaved text and new files through a successful Config rename", async () => {
  const test = await openDrawer();
  test.write.mockImplementationOnce(async () => {
    test.store.item = { ...initial, name: "renamed" };
    return accepted;
  });
  test.edit("keep unsaved text");
  fireEvent.click(screen.getByRole("button", { name: "Add file" }));
  fireEvent.change(screen.getByRole("textbox", { name: "File name" }), { target: { value: "new.yml" } });
  fireEvent.click(screen.getByRole("button", { name: "Add" }));
  await screen.findByRole("textbox", { name: "new.yml contents" });
  test.edit("new unsaved text");
  fireEvent.click(screen.getByTitle("Edit config name"));
  fireEvent.change(screen.getByRole("textbox", { name: "Edit config name" }), { target: { value: "renamed" } });
  fireEvent.click(within(screen.getByRole("dialog")).getByRole("button", { name: "Save" }));
  await screen.findByRole("button", { name: "renamed" });
  await screen.findByRole("textbox", { name: "new.yml contents" });
  expect(test.text()).toBe("new unsaved text");
  fireEvent.click(screen.getByRole("tab", { name: /config.yml/ }));
  await screen.findByRole("textbox", { name: "config.yml contents" });
  expect(test.text()).toBe("keep unsaved text");
  expect(test.write).toHaveBeenCalledWith(expect.objectContaining({ data: {
    organizationSlug: "acme", command: { command: "rename_config", environment, config: "sentry", name: "renamed" },
  } }));
});

it("isolates undo when switching to a file with the same text", async () => {
  const test = await openDrawer({ ...initial, files: [...initial.files, { name: "same.yml", bytes: 4, mode: "0444", uid: 0, gid: 0, references: [] }],
    contents: { ...initial.contents, "same.yml": "same" } });
  test.edit("same");
  fireEvent.click(screen.getByRole("tab", { name: "same.yml" }));
  await screen.findByRole("textbox", { name: "same.yml contents" });
  test.undo();
  expect(test.text()).toBe("same");
  fireEvent.click(screen.getByRole("tab", { name: /config.yml/ }));
  await screen.findByRole("textbox", { name: "config.yml contents" });
  expect(test.text()).toBe("same");
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

it("guards a pending save and retains its text when persistence is refused", async () => {
  const test = await openDrawer();
  const pending = deferred();
  test.write.mockImplementationOnce(() => pending.promise);
  test.edit("valuable unsaved text");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true));
  act(() => { void test.router.navigate({ to: "/" }); });
  const dialog = await screen.findByRole("alertdialog");
  expect(dialog.textContent).toContain("config.yml");
  fireEvent.click(within(dialog).getByRole("button", { name: "Keep editing" }));
  await act(async () => { pending.resolve(refused); });
  await screen.findByText("Unknown Config reference");
  expect(test.text()).toBe("valuable unsaved text");
  expect(test.router.state.location.pathname).toBe("/cloud/acme/shop/production/resources/config");
  act(() => { void test.router.navigate({ to: "/" }); });
  await screen.findByRole("alertdialog");
});

it("releases navigation protection once a pending save persists", async () => {
  const test = await openDrawer();
  const pending = deferred();
  test.write.mockImplementationOnce(() => pending.promise);
  test.edit("persisted text");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  test.edit("temporary edit");
  test.edit("persisted text");
  test.store.item = { ...initial, contents: { "config.yml": "persisted text" } };
  await act(async () => { pending.resolve(accepted); });
  await act(async () => { await test.router.navigate({ to: "/" }); });
  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(screen.queryByRole("textbox", { name: "config.yml contents" })).toBeNull();
});

it("lets the user explicitly discard a pending save without restoring its refused draft", async () => {
  const test = await openDrawer();
  const pending = deferred();
  test.write.mockImplementationOnce(() => pending.promise);
  test.edit("discard this text");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true));
  fireEvent.click(screen.getByRole("button", { name: "Discard" }));
  await act(async () => { pending.resolve(refused); });
  await waitFor(() => expect(test.text()).toBe("initial"));
  expect(screen.queryByText("Unknown Config reference")).toBeNull();
  await act(async () => { await test.router.navigate({ to: "/" }); });
  expect(screen.queryByRole("alertdialog")).toBeNull();
});

it.each(["retry", "discard"].flatMap((action) => ["unchanged", "newer", "returned"].map((edit) => ({ action, edit }))))("keeps a refused save guarded with $edit text when rollback reads fail until $action", async ({ action, edit }) => {
  const test = await openDrawer();
  const pending = deferred();
  test.write.mockImplementationOnce(() => pending.promise);
  test.edit("valuable offline text");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(test.write).toHaveBeenCalledTimes(1));
  await waitFor(() => expect(screen.getByRole<HTMLButtonElement>("button", { name: "Save" }).disabled).toBe(true));
  if (edit !== "unchanged") test.edit("temporary newer text");
  if (edit === "returned") test.edit("valuable offline text");
  const remainingText = test.text();
  const read = vi.mocked(functions.readStoreViewServerFn).getMockImplementation();
  if (!read) throw new Error("Store test reader did not mount");
  vi.mocked(functions.readStoreViewServerFn).mockRejectedValue(new TypeError("Failed to fetch"));
  await act(async () => { pending.resolve(refused); });
  await screen.findByText("Unknown Config reference");
  expect(test.text()).toBe(remainingText);
  act(() => { void test.router.navigate({ to: "/" }); });
  const dialog = await screen.findByRole("alertdialog");
  fireEvent.click(within(dialog).getByRole("button", { name: "Keep editing" }));
  await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
  vi.mocked(functions.readStoreViewServerFn).mockImplementation(read);
  if (action === "retry") {
    test.write.mockImplementationOnce(async () => {
      test.store.item = { ...initial, contents: { "config.yml": remainingText } };
      return accepted;
    });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(test.write).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole("button", { name: "Discard" })).toBeNull());
  } else {
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
  }
  await act(async () => { await test.router.navigate({ to: "/" }); });
  expect(screen.queryByRole("alertdialog")).toBeNull();
  expect(screen.queryByRole("textbox", { name: "config.yml contents" })).toBeNull();
});
