// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import type {
  BranchView, ConfigCommand, ConfigQuery, ConfigView, EnvironmentListing, RowId, SyncRow, SyncView,
} from "@ployz/sdk";
import type { MouseEvent } from "react";
import { toast, type Action } from "sonner";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import * as functions from "#/modules/config-store/store.functions";
import { branchQuery, environmentsQuery, storeViewPrefix, syncQuery } from "#/modules/config-store/store-view.queries";
import { SyncButton } from "./SyncButton";

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  Element.prototype.scrollIntoView ??= () => {};
});
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); document.body.replaceChildren(); });

const fixApi = { project: "shop", environment: "fix-api" };
const summary = (name: string) => ({ id: `id-${name}`, project: "shop", name, revision: 1 });
const listing = (name: string, extra: Partial<EnvironmentListing> = {}): EnvironmentListing =>
  ({ id: `id-${name}`, name, default: false, parent: null, removal: null, merged: false, branch_setup: [], ...extra });
const branchView = (extra: Partial<BranchView> = {}): BranchView => ({
  environment: summary("fix-api"), parent: "production", kept: false, setup: [], live: [], to_parent: 3,
  closes_at: Date.now() / 1000 + 5 * 24 * 60 * 60 - 60, pull_request: null, ...extra,
});
const row = (name: string, extra: Partial<SyncRow> = {}): SyncRow => ({
  row: `a:${name}` as RowId, node: "api", kind: "service", name, change: "changed", from: null, into: null, ticked: true, requires: null, secret: null, held_by: null,
  ...extra,
});
const rows = [
  row("source", { from: { type: "image", image: "shop/api:1.9", credentials: false }, into: { type: "image", image: "shop/api:1.8", credentials: false } }),
  row("env.LOG_LEVEL", { from: "debug", into: "warn", change: "conflict" }),
  row("env.APP_ENV", { from: "staging", into: "production" }),
  row("env.STRIPE_WEBHOOK_SECRET", { from: { secret: true }, change: "new", secret: {} }),
];
const neverSynced = (name: string) =>
  ({ row: `a:${name}` as RowId, node: "api", kind: "service" as const, name, marks: [{ environment: "fix-api", row: `a:${name}` as RowId }] });
const syncView = (extra: Partial<SyncView> = {}): SyncView => ({
  from: summary("fix-api"), into: summary("production"), version: "4:abc", rows,
  never_synced: [neverSynced("env.STRIPE_KEY")], proposal: null, ...extra,
});
/** What a Sync answers: its id. */
const synced = { ok: true, value: {
  written: "synced", sync: "sync-1", from: fixApi, into: { project: "shop", environment: "production" }, staged: [], closing: false, proposal: null,
} };
const pr142 = { repository_id: 1, number: 142 };

function open({ at = "fix-api", branch = branchView(), sync = syncView(), environments = [
  listing("production", { default: true }), listing("fix-api", { parent: "production" }), listing("staging"), listing("demo"),
] }: { at?: string; branch?: BranchView; sync?: SyncView; environments?: EnvironmentListing[] } = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  const success = vi.spyOn(toast, "success").mockImplementation(() => "toast");
  const seed = (query: ConfigQuery, value: ConfigView) =>
    queryClient.setQueryData([...storeViewPrefix("acme"), "session", "user", query], { ok: true, value });
  seed(branchQuery(fixApi), { view: "branch", ...branch });
  seed(environmentsQuery("shop"), { view: "environments", project: { id: "shop", name: "shop" }, environments });
  seed(syncQuery(fixApi, "production"), { view: "sync", ...sync });
  // The Sync view as the Store has it now: a test changes it to what a write leaves.
  const store = { sync };
  // What a refetch reads: that Sync view, fix-api's own views as seeded, and no Branch elsewhere.
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(({ data }) => {
    const seeded = queryClient.getQueryData([...storeViewPrefix("acme"), "session", "user", data.query]);
    // SAFETY: the views these tests read are seeded ones, the Sync view and Branch refusals.
    return Promise.resolve((data.query.query === "sync" ? { ok: true, value: { view: "sync", ...store.sync } }
      : seeded ?? { ok: false, refusal: { code: "invalid_argument", message: "Not a Branch", details: {} } }) as never);
  });
  const write = vi.spyOn(functions, "writeStoreServerFn").mockResolvedValue(synced as never);
  const commands = () => write.mock.calls.map(([call]): ConfigCommand | undefined => call?.data.command);

  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({
    getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug",
    loader: ({ params }) => ({ store: { project: params.projectSlug, environment: params.environmentSlug } }),
    component: () => <><SyncButton /><Outlet /></>,
  });
  const canvas = createRoute({ getParentRoute: () => environment, path: "/", component: () => null });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([canvas])])])])]),
    history: createMemoryHistory({ initialEntries: [`/cloud/acme/shop/${at}`] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  return { router, write, commands, success, store, queryClient };
}

async function menu() {
  fireEvent.click(await screen.findByRole("button", { name: "More sync actions" }));
  return within(await screen.findByRole("menu"));
}

async function dialog() {
  fireEvent.click(await screen.findByRole("button", { name: "Sync · 3" }));
  return within(await screen.findByRole("dialog", { name: /match fix-api$/u }));
}

/** The open dialog, made to sync into `place` instead. */
async function pick(place: string) {
  const sync = await dialog();
  fireEvent.change(sync.getByRole("combobox", { name: "Place to sync into" }), { target: { value: place } });
  await waitFor(() => expect(sync.getByRole<HTMLSelectElement>("combobox", { name: "Place to sync into" }).value).toBe(place));
  return sync;
}

it("says what a Sync into the Parent carries, and opens the dialog into the Parent", async () => {
  open();
  const sync = await dialog();
  expect(sync.getAllByRole("option").map((option) => option.textContent)).toEqual(["production", "staging", "demo"]);
  expect(sync.getByRole<HTMLSelectElement>("combobox", { name: "Place to sync into" }).value).toBe("production");
  expect(sync.getByText("These become production's changes to deploy.")).toBeTruthy();
  const changes = within(sync.getByRole("region", { name: "api" })).getAllByRole("listitem").map((item) => item.textContent);
  expect(changes).toEqual([
    "Sourceshop/api:1.81.9",
    "LOG_LEVELChanged in productionwarndebug",
    "APP_ENVproductionstaging",
    "STRIPE_WEBHOOK_SECRETSecret",
  ]);
  // A secret arrives by name only: the row takes production's own value.
  expect(sync.getByLabelText("Set production's value of STRIPE_WEBHOOK_SECRET").getAttribute("placeholder")).toBe("Set production's value");
  expect(sync.getByRole("button", { name: "Sync 4 changes" })).toBeTruthy();
});

it("reads In sync once nothing is left to sync", async () => {
  open({ branch: branchView({ to_parent: 0 }) });
  expect(await screen.findByRole("button", { name: "In sync" })).toBeTruthy();
});

it("shows a shutdown under way, and Off, ahead of the count", async () => {
  const removal = { id: "d", number: 3, status: "applied" as const, saved: 0, services: [], runner: null, upload: null, remove: true,
    admitted_by: null, admitted_at: 0, started_at: null, ended_at: null, message: null, environment_id: "id-fix-api", in_flight: false, outcome: null };
  open({ branch: branchView({ pull_request: { repository_id: 1, number: 142 } }), environments: [
    listing("production", { default: true }), listing("fix-api", { parent: "production", removal }),
  ] });
  expect(await screen.findByRole("button", { name: "Off" })).toBeTruthy();
  const items = await menu();
  // Off until the next push: a Deploy brings it back before that.
  expect(items.getByRole("menuitem", { name: "Deploy fix-api" })).toBeTruthy();
});

it("holds Keep, Closes in and Close in its menu", async () => {
  const app = open();
  const items = await menu();
  expect(items.getAllByRole("menuitem").map((item) => item.textContent)).toEqual(["Close fix-api…"]);
  expect(items.getByRole("menuitemcheckbox", { name: /^Keep fix-api\s*Closes in 5 days$/u })).toBeTruthy();
  fireEvent.click(items.getByRole("menuitemcheckbox", { name: /Keep fix-api/u }));
  await waitFor(() => expect(app.commands()).toEqual([{ command: "keep_branch", environment: fixApi, kept: true }]));
  // A Branch that isn't a PR Environment closes; it doesn't shut down.
  expect(items.queryByRole("menuitem", { name: /Shut down/u })).toBeNull();
});

it("ticks Keep at once, while the Store answers", async () => {
  const app = open();
  app.write.mockReturnValueOnce(new Promise(() => {}));
  const items = await menu();
  fireEvent.click(items.getByRole("menuitemcheckbox", { name: /Keep fix-api/u }));
  const keep = await screen.findByRole("menuitemcheckbox", { name: /Keep fix-api/u });
  await waitFor(() => expect(keep.getAttribute("aria-checked")).toBe("true"));
  // Kept, it never closes for sitting idle, nor after a Sync.
  expect(keep.textContent).toBe("Keep fix-api");
  fireEvent.keyDown(keep, { key: "Escape" });
  const sync = await dialog();
  expect(sync.queryByRole("checkbox", { name: /Close fix-api/u })).toBeNull();
});

it("syncs sideways from the picker, without offering to close the Branch", async () => {
  open();
  const sync = await pick("staging");
  expect(sync.getByText("These become staging's changes to deploy.")).toBeTruthy();
  expect(sync.queryByRole("checkbox", { name: /Close fix-api/u })).toBeNull();
});

it("marks an unticked change Never sync, and syncs a never-synced one again", async () => {
  const app = open();
  const sync = await dialog();
  fireEvent.click(sync.getByRole("checkbox", { name: "APP_ENV" }));
  expect(sync.getByRole("button", { name: "Sync 3 changes" })).toBeTruthy();
  // What the Store answers once it's marked.
  app.store.sync = syncView({ rows: rows.filter(({ name }) => name !== "env.APP_ENV"), never_synced: [
    ...syncView().never_synced, neverSynced("env.APP_ENV"),
  ] });
  fireEvent.click(sync.getByRole("button", { name: "Never sync" }));
  expect(await sync.findByRole("button", { name: "2 never synced" })).toBeTruthy();
  expect(sync.queryByText("APP_ENV")).toBeNull();
  await waitFor(() => expect(app.commands()).toEqual([
    { command: "never_sync", environment: fixApi, rows: ["a:env.APP_ENV"], off: false },
  ]));
  fireEvent.click(sync.getByRole("button", { name: "2 never synced" }));
  const list = within(await sync.findByRole("list", { name: "Never synced" }));
  fireEvent.click(within(list.getByText("STRIPE_KEY").closest("li") ?? document.body).getByRole("button", { name: "Sync again" }));
  await waitFor(() => expect(app.commands()[1]).toEqual(
    { command: "never_sync", environment: fixApi, rows: ["a:env.STRIPE_KEY"], off: true }));
});

it("syncs what's ticked, closes the Branch after, lands on the receiver, and Undo undoes that Sync", async () => {
  const app = open();
  const sync = await dialog();
  fireEvent.click(sync.getByRole("checkbox", { name: "APP_ENV" }));
  expect(sync.getByRole("checkbox", { name: "Close fix-api after syncing" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(sync.getByRole("button", { name: "Sync 3 changes" }));
  await waitFor(() => expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/production"));
  expect(app.commands()).toEqual([{
    command: "sync", from: fixApi, into: { project: "shop", environment: "production" }, version: "4:abc",
    close_after: true, picks: ["a:source", "a:env.LOG_LEVEL", "a:env.STRIPE_WEBHOOK_SECRET"], values: {},
    id: expect.any(String),
  }]);
  expect(app.success.mock.calls.at(0)?.[0]).toBe("Synced 3 changes from fix-api");
  // SAFETY: the Sync button's toast action is a label and a click, never a node.
  const action = app.success.mock.calls.at(0)?.[1]?.action as Action | undefined;
  expect(action?.label).toBe("Undo");
  action?.onClick(asTestDouble<MouseEvent<HTMLButtonElement>>()({}));
  await waitFor(() => expect(app.commands().slice(1)).toEqual([{ command: "undo_sync", sync: "sync-1" }]));
});

it("keeps a kept Branch open after syncing: no Close checkbox", async () => {
  open({ branch: branchView({ kept: true, closes_at: null }) });
  const sync = await dialog();
  expect(sync.queryByRole("checkbox", { name: /Close fix-api/u })).toBeNull();
});

it("syncs a PR Environment into production now and opens production, with no Close", async () => {
  const app = open({ branch: branchView({ pull_request: pr142 }) });
  const sync = await dialog();
  expect(sync.getByText("These become production's changes to deploy.")).toBeTruthy();
  // A PR Environment closes with its pull request.
  expect(sync.queryByRole("checkbox", { name: /Close fix-api/u })).toBeNull();
  fireEvent.change(sync.getByLabelText("Set production's value of STRIPE_WEBHOOK_SECRET"), { target: { value: "whsec" } });
  fireEvent.click(sync.getByRole("button", { name: "Sync 4 changes" }));
  await waitFor(() => expect(app.success).toHaveBeenCalled());
  const production = { project: "shop", environment: "production" };
  expect(app.commands()[0]).toMatchObject({
    command: "sync", from: fixApi, into: production, close_after: false,
    values: { "a:env.STRIPE_WEBHOOK_SECRET": "whsec" },
  });
  expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/production");
  expect(app.success.mock.calls.at(0)?.[0]).toBe("Synced 4 changes from fix-api");
});

it("syncs a PR Environment into another Environment now, and its menu shuts it down", async () => {
  const app = open({ branch: branchView({ pull_request: pr142 }) });
  const items = await menu();
  expect(items.getByRole("menuitem", { name: "Shut down until the next push" })).toBeTruthy();
  fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
  const sync = await pick("staging");
  fireEvent.change(sync.getByLabelText("Set staging's value of STRIPE_WEBHOOK_SECRET"), { target: { value: "whsec" } });
  fireEvent.click(sync.getByRole("button", { name: "Sync 4 changes" }));
  await waitFor(() => expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/staging"));
  // A value set now seals the secret, with the Sync.
  const staging = { project: "shop", environment: "staging" };
  expect(app.commands()).toEqual([{
    command: "sync", from: fixApi, into: staging, close_after: false, version: "4:abc",
    picks: ["a:source", "a:env.LOG_LEVEL", "a:env.APP_ENV", "a:env.STRIPE_WEBHOOK_SECRET"],
    values: { "a:env.STRIPE_WEBHOOK_SECRET": "whsec" }, id: expect.any(String),
  }]);
  expect(app.success.mock.calls.at(0)?.[0]).toBe("Synced 4 changes from fix-api");
});

it("shows the fresh rows when the review went stale, and stays open", async () => {
  const app = open();
  app.write.mockResolvedValueOnce({ ok: false, refusal: { code: "conflict", message: "stale", details: { version: "5:def" } } } as never);
  app.store.sync = syncView({ version: "5:def", rows: rows.slice(0, 1) });
  const sync = await dialog();
  fireEvent.click(sync.getByRole("button", { name: "Sync 4 changes" }));
  expect(await sync.findByText("These changed since you opened them. Here they are now.")).toBeTruthy();
  expect(await sync.findByRole("button", { name: "Sync 1 change" })).toBeTruthy();
  expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/fix-api");
  expect(toast.error).not.toHaveBeenCalled();
});

it("syncs from an Environment that isn't a Branch with Sync alone, picking any other place", async () => {
  const app = open({ at: "production" });
  fireEvent.click(await screen.findByRole("button", { name: "Sync" }));
  expect(screen.queryByRole("button", { name: "More sync actions" })).toBeNull();
  const sync = within(await screen.findByRole("dialog", { name: /match production$/u }));
  expect(sync.getAllByRole("option").map((option) => option.textContent)).toEqual(["fix-api", "staging", "demo"]);
  fireEvent.change(sync.getByRole("combobox", { name: "Place to sync into" }), { target: { value: "staging" } });
  expect(await sync.findByText("These become staging's changes to deploy.")).toBeTruthy();
  fireEvent.click(sync.getByRole("button", { name: "Sync 4 changes" }));
  await waitFor(() => expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/staging"));
  expect(app.commands()[0]).toMatchObject({
    command: "sync", from: { project: "shop", environment: "production" }, into: { project: "shop", environment: "staging" }, close_after: false,
  });
});
