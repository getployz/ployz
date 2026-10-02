// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import type {
  BranchView, ConfigCommand, ConfigQuery, ConfigView, EnvironmentListing, PullRequestView, RowId, SyncRow, SyncView,
} from "@ployz/sdk";
import type { MouseEvent } from "react";
import { toast, type Action } from "sonner";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import * as functions from "#/modules/config-store/store.functions";
import { pullRequestQuery } from "#/modules/config-store/store-pull-requests";
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
  ({ id: `id-${name}`, name, default: false, parent: null, removal: null, branch_setup: [], ...extra });
const branchView = (extra: Partial<BranchView> = {}): BranchView => ({
  environment: summary("fix-api"), parent: "production", kept: false, setup: [], live: [], to_parent: 3,
  closes_at: Date.now() / 1000 + 5 * 24 * 60 * 60 - 60, pull_request: null, ...extra,
});
const row = (name: string, extra: Partial<SyncRow> = {}): SyncRow => ({
  row: `a:${name}` as RowId, node: "api", name, change: "changed", from: null, into: null, ticked: true, requires: null, secret: null,
  ...extra,
});
const rows = [
  row("image", { from: "shop/api:1.9", into: "shop/api:1.8" }),
  row("env.LOG_LEVEL", { from: "debug", into: "warn", change: "conflict" }),
  row("env.APP_ENV", { from: "staging", into: "production" }),
  row("env.STRIPE_WEBHOOK_SECRET", { from: { secret: true }, change: "new", secret: { needs_value: true, held: false } }),
];
const neverSynced = (name: string) =>
  ({ row: `a:${name}` as RowId, node: "api", name, marks: [{ environment: "fix-api", row: `a:${name}` as RowId }] });
const syncView = (extra: Partial<SyncView> = {}): SyncView => ({
  from: summary("fix-api"), into: summary("production"), at_merge: null, version: "4:abc", rows,
  never_synced: [neverSynced("env.STRIPE_KEY")], ...extra,
});
/** What a Sync answers: its id, and the Conditional Sync standing for one at the merge. */
const synced = (atMerge: number | null) => ({ ok: true, value: {
  written: "synced", sync: "sync-1",
  when: atMerge === null ? { kind: "now", staged: [], closing: false }
    : { kind: "at_merge", conditional_sync: { id: "cs-new", pull_request: atMerge, rows: [], state: "standing" } },
} });
const pr142 = { repository_id: 1, number: 142 };
/** PR #142's view: fix-api is its PR Environment, production its Destination with 3 changes; `synced`, they stand there. */
const pullRequestView = (synced: boolean): PullRequestView => ({
  pull_request: {
    ...pr142, title: "Add search", author: "ada", bot: false, head_branch: "search", head: "1".repeat(40), target_branch: "main",
    commits: 1, open: true, merge_commit: null, merge_reached: null, updated: "2026-09-29T10:00:00Z",
  },
  environments: [{ environment: summary("fix-api"), deployment: null, destinations: [{
    name: "production", changes: 3, conditional_sync: synced ? { id: "cs", standing: true, changes: 3, waiting: [] } : null,
  }] }],
  passing: synced, reason: synced ? "3 changes go live with this PR" : "3 changes to sync in Ployz",
});

function open({ branch = branchView(), sync = syncView(), pullRequest = null, environments = [
  listing("production", { default: true }), listing("fix-api", { parent: "production" }), listing("staging"), listing("demo"),
] }: { branch?: BranchView; sync?: SyncView; pullRequest?: PullRequestView | null; environments?: EnvironmentListing[] } = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  vi.spyOn(toast, "error").mockImplementation(() => "toast");
  const success = vi.spyOn(toast, "success").mockImplementation(() => "toast");
  const seed = (query: ConfigQuery, value: ConfigView) =>
    queryClient.setQueryData([...storeViewPrefix("acme"), "session", "user", query], { ok: true, value });
  seed(branchQuery(fixApi), { view: "branch", ...branch });
  seed(environmentsQuery("shop"), { view: "environments", project: { id: "shop", name: "shop" }, environments });
  seed(syncQuery(fixApi, "production"), { view: "sync", ...sync });
  if (pullRequest) seed(pullRequestQuery(pr142), { view: "pull_request", ...pullRequest });
  // The Sync view as the Store has it now: a test changes it to what a write leaves.
  const store = { sync };
  // What a refetch reads: that Sync view, fix-api's own views as seeded, and no Branch elsewhere.
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(({ data }) => {
    const seeded = queryClient.getQueryData([...storeViewPrefix("acme"), "session", "user", data.query]);
    // SAFETY: the views these tests read are seeded ones, the Sync view and Branch refusals.
    return Promise.resolve((data.query.query === "sync" ? { ok: true, value: { view: "sync", ...store.sync } }
      : seeded ?? { ok: false, refusal: { code: "invalid_argument", message: "Not a Branch", details: {} } }) as never);
  });
  const write = vi.spyOn(functions, "writeStoreServerFn").mockResolvedValue(synced(sync.at_merge) as never);
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
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/fix-api"] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  return { router, write, commands, success, store, queryClient };
}

async function menu() {
  fireEvent.click(await screen.findByRole("button", { name: "More sync actions" }));
  return within(await screen.findByRole("menu"));
}

async function dialog() {
  fireEvent.click(await screen.findByRole("button", { name: "Sync to production · 3" }));
  return within(await screen.findByRole("dialog", { name: "Sync to production" }));
}

it("says what a Sync into the Parent carries, and opens the dialog into the Parent", async () => {
  open();
  const sync = await dialog();
  expect(sync.getByText("These changes from fix-api become production's changes to deploy.")).toBeTruthy();
  const changes = within(sync.getByRole("region", { name: "api" })).getAllByRole("listitem").map((item) => item.textContent);
  expect(changes).toEqual([
    "Container imageshop/api:1.8shop/api:1.9",
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
  expect(await screen.findByRole("button", { name: "In sync with production" })).toBeTruthy();
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

it("syncs to every other Environment from its menu, and holds Keep, Closes in and Close", async () => {
  const app = open();
  const items = await menu();
  expect(items.getAllByRole("menuitem").map((item) => item.textContent)).toEqual([
    "Sync to production3 changes", "Sync to staging", "Sync to demo", "Close fix-api…",
  ]);
  expect(items.getByRole("menuitemcheckbox", { name: /^Keep fix-api\s*Closes in 5 days$/u })).toBeTruthy();
  fireEvent.click(items.getByRole("menuitemcheckbox", { name: /Keep fix-api/u }));
  await waitFor(() => expect(app.commands()).toEqual([{ command: "keep_branch", environment: fixApi, kept: true }]));
  // A Branch that isn't a PR Environment closes; it doesn't shut down.
  expect(items.queryByRole("menuitem", { name: /Shut down/u })).toBeNull();
});

it("syncs sideways from the menu, without offering to close the Branch", async () => {
  open();
  fireEvent.click((await menu()).getByRole("menuitem", { name: "Sync to staging" }));
  const sync = within(await screen.findByRole("dialog", { name: "Sync to staging" }));
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
  // Once the Store answers: the row joins the never-synced list.
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
    when: { kind: "now", close_after: true }, picks: ["a:image", "a:env.LOG_LEVEL", "a:env.STRIPE_WEBHOOK_SECRET"], values: {},
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

it("syncs a PR Environment into its Destination at the merge, staying put, and Undo withdraws it", async () => {
  const app = open({ branch: branchView({ pull_request: pr142 }), sync: syncView({ at_merge: 142 }), pullRequest: pullRequestView(false) });
  const sync = await dialog();
  expect(sync.getByText("These changes from fix-api go live in production when #142 merges.")).toBeTruthy();
  // A PR Environment closes with its pull request.
  expect(sync.queryByRole("checkbox", { name: /Close fix-api/u })).toBeNull();
  fireEvent.change(sync.getByLabelText("Set production's value of STRIPE_WEBHOOK_SECRET"), { target: { value: "whsec" } });
  fireEvent.click(sync.getByRole("button", { name: "Sync 4 changes" }));
  await waitFor(() => expect(app.success).toHaveBeenCalled());
  // The value is held for the merge, with the Sync.
  const production = { project: "shop", environment: "production" };
  expect(app.commands()[0]).toMatchObject({
    command: "sync", from: fixApi, into: production, when: null,
    values: { "a:env.STRIPE_WEBHOOK_SECRET": "whsec" },
  });
  expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/fix-api");
  expect(app.success.mock.calls.at(0)?.[0]).toBe("4 changes go live in production when #142 merges");
  // SAFETY: the Sync button's toast action is a label and a click, never a node.
  const action = app.success.mock.calls.at(0)?.[1]?.action as Action | undefined;
  action?.onClick(asTestDouble<MouseEvent<HTMLButtonElement>>()({}));
  await waitFor(() => expect(app.commands()[1]).toEqual({ command: "undo_sync", sync: "sync-1" }));
});

it("reads Goes live with #N once a Conditional Sync stands, with the GitHub check and Undo in its menu", async () => {
  const app = open({ branch: branchView({ pull_request: pr142 }), pullRequest: pullRequestView(true) });
  expect(await screen.findByRole("button", { name: "Goes live with #142" })).toBeTruthy();
  const items = await menu();
  expect(items.getByText("Ready to merge on GitHub")).toBeTruthy();
  expect(items.getByText("3 changes go live with this PR")).toBeTruthy();
  expect(items.getByRole("menuitem", { name: "Shut down until the next push" })).toBeTruthy();
  fireEvent.click(items.getByRole("menuitem", { name: "Undo sync to production" }));
  // The standing Conditional Sync's id names it.
  await waitFor(() => expect(app.commands()).toEqual([{ command: "undo_sync", sync: "cs" }]));
});

it("syncs a PR Environment into another Environment now, from the menu", async () => {
  const app = open({ branch: branchView({ pull_request: pr142 }), pullRequest: pullRequestView(true) });
  fireEvent.click((await menu()).getByRole("menuitem", { name: "Sync to staging" }));
  const sync = within(await screen.findByRole("dialog", { name: "Sync to staging" }));
  expect(sync.getByText("These changes from fix-api become staging's changes to deploy.")).toBeTruthy();
  fireEvent.change(sync.getByLabelText("Set staging's value of STRIPE_WEBHOOK_SECRET"), { target: { value: "whsec" } });
  fireEvent.click(sync.getByRole("button", { name: "Sync 4 changes" }));
  await waitFor(() => expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/staging"));
  // A value set now seals the secret, with the Sync.
  const staging = { project: "shop", environment: "staging" };
  expect(app.commands()).toEqual([{
    command: "sync", from: fixApi, into: staging, when: null, version: "4:abc",
    picks: ["a:image", "a:env.LOG_LEVEL", "a:env.APP_ENV", "a:env.STRIPE_WEBHOOK_SECRET"],
    values: { "a:env.STRIPE_WEBHOOK_SECRET": "whsec" },
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
