// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConfigQuery, ConfigView, EnvironmentListing } from "@ployz/sdk";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider, useParams } from "@tanstack/react-router";
import { environmentsQuery, projectsQuery, storeViewPrefix } from "#/modules/config-store/store-view.queries";
import { Crumbs, EnvironmentCrumbs } from "./environment-breadcrumbs";

beforeEach(() => {
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Element.prototype.scrollIntoView ??= () => {};
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

function TopBar() {
  const { organizationSlug, projectSlug, environmentSlug } = useParams({ strict: false });
  if (!organizationSlug || !projectSlug || !environmentSlug) return null;
  // Like the shell's: the bar outlives the page.
  return <EnvironmentCrumbs scope={{ kind: "environment", organizationSlug, projectSlug, environmentSlug }} />;
}

const listing = (name: string, extra: Partial<EnvironmentListing> = {}): EnvironmentListing =>
  ({ id: `id-${name}`, name, default: false, parent: null, removal: null, branch_setup: [], ...extra });

async function renderAt(path: string) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  const seed = (query: ConfigQuery, value: ConfigView) =>
    queryClient.setQueryData([...storeViewPrefix("acme"), "session", "user", query], { ok: true, value });
  seed(projectsQuery(), { view: "projects", projects: [
    { id: "store", name: "store", default_environment: "production", environments: ["fix-web", "pr-142", "production", "staging"] },
    { id: "docs", name: "docs", default_environment: "preview", environments: ["preview", "production"] },
  ] });
  seed(environmentsQuery("store"), { view: "environments", project: { id: "store", name: "store" }, environments: [
    listing("fix-web", { parent: "production" }),
    listing("pr-142", { parent: "staging", removal: { id: "d", number: 3, status: "running", saved: 0, services: [], runner: null, upload: null, remove: true, admitted_by: null, admitted_at: 0, started_at: null, ended_at: null, message: null, environment_id: "pr-142", in_flight: true, outcome: null } }),
    listing("production", { default: true }),
    listing("staging"),
  ] });
  seed(environmentsQuery("docs"), { view: "environments", project: { id: "docs", name: "docs" }, environments: [
    listing("preview", { default: true }), listing("production"),
  ] });
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }), component: Outlet });
  const organizationRoute = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: () => <><TopBar /><Outlet /></> });
  const projectGroup = createRoute({ getParentRoute: () => organizationRoute, id: "_project" });
  const environment = createRoute({ getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug" });
  const logs = createRoute({ getParentRoute: () => environment, path: "logs" });
  const settings = createRoute({ getParentRoute: () => environment, path: "settings" });
  const organizationGroup = createRoute({ getParentRoute: () => organizationRoute, id: "_org" });
  const organizationHome = createRoute({ getParentRoute: () => organizationGroup, path: "~" });
  const routeTree = root.addChildren([protectedRoute.addChildren([organizationRoute.addChildren([projectGroup.addChildren([environment.addChildren([logs, settings])]), organizationGroup.addChildren([organizationHome])])])]);
  const router = createRouter({ routeTree, history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByRole("button", { name: "Project: store" });
  return {
    router,
    [Symbol.asyncDispose]() {
      cleanup();
      queryClient.clear();
      return Promise.resolve();
    },
  };
}

it("switches project from its crumb, noting the Environment each opens, and keeps the place", async () => {
  await using app = await renderAt("/cloud/acme/store/staging/logs");
  expect(screen.getByRole("button", { name: "Environment: staging" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Project: store" }));
  expect(await screen.findByRole("option", { name: "store, opens production" })).toBeTruthy();
  fireEvent.click(screen.getByRole("option", { name: "docs, opens preview" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/docs/preview/logs"));
  expect(await screen.findByRole("button", { name: "Environment: preview" })).toBeTruthy();
});

it("closes the More menu once a project picked in it opens", async () => {
  await using app = await renderAt("/cloud/acme/store/staging/settings");
  fireEvent.click(screen.getByRole("button", { name: "More breadcrumbs" }));
  const menu = await screen.findByRole("dialog", { name: "More breadcrumbs" });
  fireEvent.click(within(menu).getByRole("button", { name: "Project: store" }));
  fireEvent.click(await screen.findByRole("option", { name: "docs, opens preview" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/docs/preview/settings"));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "More breadcrumbs" })).toBeNull());
});

it("opens Projects from All projects", async () => {
  await using app = await renderAt("/cloud/acme/store/production/logs");
  fireEvent.click(screen.getByRole("button", { name: "Project: store" }));
  fireEvent.click(await screen.findByRole("option", { name: "All projects" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/~"));
});

it("switches between this project's Environments and keeps the place", async () => {
  await using app = await renderAt("/cloud/acme/store/production/logs");
  fireEvent.click(screen.getByRole("button", { name: "Environment: production" }));
  expect(await screen.findByRole("option", { name: "staging" })).toBeTruthy();
  expect(screen.queryByRole("option", { name: "preview, default" })).toBeNull();
  fireEvent.click(screen.getByRole("option", { name: "staging" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/store/staging/logs"));
});

it("lists Environments as a tree, each Branch under its Parent, noting the default and a removal under way", async () => {
  await using _app = await renderAt("/cloud/acme/store/production/logs");
  fireEvent.click(screen.getByRole("button", { name: "Environment: production" }));
  await screen.findByRole("option", { name: "production, default" });
  expect(screen.getAllByRole("option").map((option) => option.getAttribute("aria-label") ?? option.textContent)).toEqual([
    "production, default", "fix-web, branch of production", "staging", "pr-142, branch of staging, deleting",
    "New branch of production", "Manage environments",
  ]);
});

it("offers New branch of the current Environment, which opens the panel over its canvas", async () => {
  await using app = await renderAt("/cloud/acme/store/production/logs");
  fireEvent.click(screen.getByRole("button", { name: "Environment: production" }));
  fireEvent.click(await screen.findByRole("option", { name: "New branch of production" }));
  await waitFor(() => expect(app.router.state.location.pathname).toBe("/cloud/acme/store/production/new-branch"));
});

it("reads project / Parent ⑂ Branch on a Branch, and the Parent's crumb opens the Parent in the same place", async () => {
  await using app = await renderAt("/cloud/acme/store/fix-web/logs");
  expect(screen.getByRole("button", { name: "Environment: fix-web" })).toBeTruthy();
  // Phones fold all but the last two crumbs into a menu: the Branch's own crumb stays out of it.
  fireEvent.click(screen.getByRole("button", { name: "More breadcrumbs" }));
  const menu = await screen.findByRole("dialog", { name: "More breadcrumbs" });
  expect(within(menu).getByRole("link", { name: "Parent: production" })).toBeTruthy();
  expect(within(menu).queryByRole("button", { name: "Environment: fix-web" })).toBeNull();
  fireEvent.click(within(menu).getByRole("link", { name: "Parent: production" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/store/production/logs"));
  expect(screen.queryByRole("link", { name: /Parent/u })).toBeNull();
});

it("keeps every crumb but the last two in a More menu", async () => {
  const root = createRootRoute({ component: () => <Crumbs items={[<a key="a" href="/a">First</a>, <a key="b" href="/b">Second</a>, <a key="c" href="/c">Third</a>]} /> });
  render(<RouterProvider router={createRouter({ routeTree: root, history: createMemoryHistory() })} />);
  fireEvent.click(await screen.findByRole("button", { name: "More breadcrumbs" }));
  const menu = await screen.findByRole("dialog", { name: "More breadcrumbs" });
  expect(within(menu).getAllByRole("link").map((link) => link.textContent)).toEqual(["First"]);
});

it("notes the Default Environment and opens the Project settings from Manage environments", async () => {
  await using app = await renderAt("/cloud/acme/store/staging/logs");
  fireEvent.click(screen.getByRole("button", { name: "Environment: staging" }));
  expect(await screen.findByRole("option", { name: "production, default" })).toBeTruthy();
  fireEvent.click(screen.getByRole("option", { name: "Manage environments" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/store/staging/settings?scope=project"));
});

it("offers Manage of the current Branch", async () => {
  await using app = await renderAt("/cloud/acme/store/fix-web/logs");
  fireEvent.click(screen.getByRole("button", { name: "Environment: fix-web" }));
  fireEvent.click(await screen.findByRole("option", { name: "Manage fix-web" }));
  await waitFor(() => expect(app.router.state.location.pathname).toBe("/cloud/acme/store/fix-web/review"));
});
