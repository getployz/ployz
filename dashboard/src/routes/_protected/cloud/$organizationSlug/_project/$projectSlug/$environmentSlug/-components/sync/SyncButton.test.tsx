// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import type {
  BranchView, ConfigCommand, ConfigQuery, ConfigView, EnvironmentListing, PullRequestView, SyncRow, SyncView,
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
const row = (key: string, label: string, extra: Partial<SyncRow> = {}): SyncRow =>
  ({ key, node: "api", label, from: null, into: null, ticked: true, changed: false, new: false, secret: false, ...extra });
const rows = [
  row("a:source.image", "api.image", { from: "shop/api:1.9", into: "shop/api:1.8" }),
  row("a:variables.LOG_LEVEL", "api.env.LOG_LEVEL", { from: "debug", into: "warn", changed: true }),
  row("a:variables.APP_ENV", "api.env.APP_ENV", { from: "staging", into: "production" }),
  row("a:variables.STRIPE_WEBHOOK_SECRET", "api.env.STRIPE_WEBHOOK_SECRET", { from: { secret: true }, new: true, secret: true }),
];
const syncView = (extra: Partial<SyncView> = {}): SyncView => ({
  from: summary("fix-api"), into: summary("production"), at_merge: null, version: "4:abc", rows,
  never_synced: [{ key: "a:variables.STRIPE_KEY", node: "api", label: "api.env.STRIPE_KEY", marked_in: ["fix-api"] }], ...extra,
});
const pr142 = { repository_id: 1, number: 142 };
/** PR #142's view: fix-api is its PR Environment, production its Destination with 3 changes; `synced`, they stand there. */
const pullRequestView = (synced: boolean): PullRequestView => ({
  pull_request: {
    ...pr142, title: "Add search", author: "ada", bot: false, head_branch: "search", head: "1".repeat(40), target_branch: "main",
    commits: 1, open: true, merge_commit: null, merge_reached: null, updated: "2026-09-29T10:00:00Z",
  },
  environments: [{ environment: summary("fix-api"), deployment: null, destinations: [{
    name: "production", changes: 3, conditional_sync: synced ? { id: "cs", standing: true, changes: 3 } : null,
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
  seed(syncQuery(fixApi), { view: "sync", ...sync });
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
  const write = vi.spyOn(functions, "writeStoreServerFn").mockResolvedValue({ ok: true, value: { written: "synced" } } as never);
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
  return { router, write, commands, success, store };
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
    "STRIPE_WEBHOOK_SECRETSecretValue set in production",
  ]);
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
  app.store.sync = syncView({ rows: rows.filter(({ label }) => label !== "api.env.APP_ENV"), never_synced: [
    ...syncView().never_synced, { key: "a:variables.APP_ENV", node: "api", label: "api.env.APP_ENV", marked_in: ["fix-api"] },
  ] });
  fireEvent.click(sync.getByRole("button", { name: "Never sync" }));
  // At once: the row joins the never-synced list.
  expect(await sync.findByRole("button", { name: "2 never synced" })).toBeTruthy();
  expect(sync.queryByText("APP_ENV")).toBeNull();
  await waitFor(() => expect(app.commands()).toEqual([
    { command: "never_sync", environment: fixApi, paths: ["api.env.APP_ENV"], off: false },
  ]));
  fireEvent.click(sync.getByRole("button", { name: "2 never synced" }));
  const list = within(await sync.findByRole("list", { name: "Never synced" }));
  fireEvent.click(within(list.getByText("STRIPE_KEY").closest("li") ?? document.body).getByRole("button", { name: "Sync again" }));
  await waitFor(() => expect(app.commands()[1]).toEqual(
    { command: "never_sync", environment: fixApi, paths: ["api.env.STRIPE_KEY"], off: true }));
});

it("syncs what's ticked, closes the Branch after, lands on the receiver, and Undo discards what synced", async () => {
  const app = open();
  const sync = await dialog();
  fireEvent.click(sync.getByRole("checkbox", { name: "APP_ENV" }));
  expect(sync.getByRole("checkbox", { name: "Close fix-api after syncing" }).getAttribute("aria-checked")).toBe("true");
  fireEvent.click(sync.getByRole("button", { name: "Sync 3 changes" }));
  await waitFor(() => expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/production"));
  expect(app.commands()).toEqual([{
    command: "sync", from: fixApi, into: { project: "shop", environment: "production" }, version: "4:abc", close_after: true,
    picks: ["a:source.image", "a:variables.LOG_LEVEL", "a:variables.STRIPE_WEBHOOK_SECRET"],
  }]);
  expect(app.success.mock.calls.at(0)?.[0]).toBe("Synced 3 changes from fix-api");
  // SAFETY: the Sync button's toast action is a label and a click, never a node.
  const action = app.success.mock.calls.at(0)?.[1]?.action as Action | undefined;
  expect(action?.label).toBe("Undo");
  action?.onClick(asTestDouble<MouseEvent<HTMLButtonElement>>()({}));
  const production = { project: "shop", environment: "production" };
  await waitFor(() => expect(app.commands().slice(1)).toEqual(
    ["api.image", "api.env.LOG_LEVEL", "api.env.STRIPE_WEBHOOK_SECRET"].map((path) => ({ command: "discard", environment: production, path, version: null }))));
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
  fireEvent.click(sync.getByRole("button", { name: "Sync 4 changes" }));
  await waitFor(() => expect(app.success).toHaveBeenCalled());
  expect(app.commands()[0]).toMatchObject({ command: "sync", from: fixApi, close_after: false });
  expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/fix-api");
  expect(app.success.mock.calls.at(0)?.[0]).toBe("4 changes go live in production when #142 merges");
  // SAFETY: the Sync button's toast action is a label and a click, never a node.
  const action = app.success.mock.calls.at(0)?.[1]?.action as Action | undefined;
  action?.onClick(asTestDouble<MouseEvent<HTMLButtonElement>>()({}));
  await waitFor(() => expect(app.commands()[1]).toEqual({
    command: "sync", from: fixApi, into: { project: "shop", environment: "production" }, picks: [], when: "at_merge",
  }));
});

it("reads Goes live with #N once a Conditional Sync stands, with the GitHub check and Undo in its menu", async () => {
  const app = open({ branch: branchView({ pull_request: pr142 }), pullRequest: pullRequestView(true) });
  expect(await screen.findByRole("button", { name: "Goes live with #142" })).toBeTruthy();
  const items = await menu();
  expect(items.getByText("Ready to merge on GitHub")).toBeTruthy();
  expect(items.getByText("3 changes go live with this PR")).toBeTruthy();
  expect(items.getByRole("menuitem", { name: "Shut down until the next push" })).toBeTruthy();
  fireEvent.click(items.getByRole("menuitem", { name: "Undo sync to production" }));
  await waitFor(() => expect(app.commands()).toEqual([{
    command: "sync", from: fixApi, into: { project: "shop", environment: "production" }, picks: [], when: "at_merge",
  }]));
});

it("syncs a PR Environment into another Environment now, from the menu", async () => {
  const app = open({ branch: branchView({ pull_request: pr142 }), pullRequest: pullRequestView(true) });
  fireEvent.click((await menu()).getByRole("menuitem", { name: "Sync to staging" }));
  const sync = within(await screen.findByRole("dialog", { name: "Sync to staging" }));
  expect(sync.getByText("These changes from fix-api become staging's changes to deploy.")).toBeTruthy();
  fireEvent.click(sync.getByRole("button", { name: "Sync 4 changes" }));
  await waitFor(() => expect(app.router.state.location.pathname).toBe("/cloud/acme/shop/staging"));
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
