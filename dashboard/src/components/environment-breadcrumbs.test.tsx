// @vitest-environment jsdom
import { orgStoreSeed } from "#/test/org-store-tables";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider, useParams } from "@tanstack/react-router";
import { getEnvironmentSummariesCollection } from "#/collections/collections";
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
  return <EnvironmentCrumbs key={`${projectSlug}/${environmentSlug}`} scope={{ kind: "environment", organizationSlug, projectSlug, environmentSlug }} />;
}

async function renderAt(path: string) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  const key = (table: string) => ["collections", "session", "user", "acme", table];
  queryClient.setQueryData(key("project"), orgStoreSeed([
    { id: "store", slug: "store", name: "Store" },
    { id: "docs", slug: "docs", name: "Docs" },
  ]));
  queryClient.setQueryData(key("environment_summary"), orgStoreSeed([
    { createdAt: new Date(0), id: "store-production", projectId: "store", namespace: "production", name: "Production" },
    { createdAt: new Date(1), id: "store-staging", projectId: "store", namespace: "staging", name: "Staging" },
    { createdAt: new Date(1), id: "docs-production", projectId: "docs", namespace: "production", name: "Docs production" },
    { createdAt: new Date(0), id: "docs-preview", projectId: "docs", namespace: "preview", name: "Docs preview" },
  ]));
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
  await screen.findByRole("button", { name: "Project: Store" });
  return {
    router,
    async [Symbol.asyncDispose]() {
      cleanup();
      await getEnvironmentSummariesCollection("acme", scope).cleanup();
      queryClient.clear();
    },
  };
}

it("switches project from its crumb, noting the Environment each opens, and keeps the place", async () => {
  await using app = await renderAt("/cloud/acme/store/staging/logs");
  expect(screen.getByRole("button", { name: "Environment: Staging" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Project: Store" }));
  expect(await screen.findByRole("option", { name: "Store, opens Production" })).toBeTruthy();
  fireEvent.click(screen.getByRole("option", { name: "Docs, opens Docs preview" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/docs/preview/logs"));
  expect(await screen.findByRole("button", { name: "Environment: Docs preview" })).toBeTruthy();
});

it("opens Projects from All projects", async () => {
  await using app = await renderAt("/cloud/acme/store/production/logs");
  fireEvent.click(screen.getByRole("button", { name: "Project: Store" }));
  fireEvent.click(await screen.findByRole("option", { name: "All projects" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/~"));
});

it("switches between this project's Environments and keeps the place", async () => {
  await using app = await renderAt("/cloud/acme/store/production/logs");
  fireEvent.click(screen.getByRole("button", { name: "Environment: Production" }));
  expect(await screen.findByRole("option", { name: "Staging" })).toBeTruthy();
  expect(screen.queryByRole("option", { name: "Docs preview" })).toBeNull();
  fireEvent.click(screen.getByRole("option", { name: "Staging" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/store/staging/logs"));
});

it("opens the create dialog from New environment", async () => {
  await using _app = await renderAt("/cloud/acme/store/production/logs");
  fireEvent.click(screen.getByRole("button", { name: "Environment: Production" }));
  fireEvent.click(await screen.findByRole("option", { name: "New environment" }));
  expect(await screen.findByRole("dialog", { name: "Add environment" })).toBeTruthy();
});

it("keeps every crumb but the last two in a More menu", async () => {
  render(<Crumbs items={[<a key="a" href="/a">First</a>, <a key="b" href="/b">Second</a>, <a key="c" href="/c">Third</a>]} />);
  fireEvent.click(screen.getByRole("button", { name: "More breadcrumbs" }));
  const menu = await screen.findByRole("dialog", { name: "More breadcrumbs" });
  expect(within(menu).getAllByRole("link").map((link) => link.textContent)).toEqual(["First"]);
});

it("notes the Default Environment and opens the Project settings from Manage environments", async () => {
  await using app = await renderAt("/cloud/acme/store/staging/logs");
  fireEvent.click(screen.getByRole("button", { name: "Environment: Staging" }));
  expect(await screen.findByRole("option", { name: "Production, default" })).toBeTruthy();
  fireEvent.click(screen.getByRole("option", { name: "Manage environments" }));
  await waitFor(() => expect(app.router.state.location.href).toBe("/cloud/acme/store/staging/settings?scope=project"));
});
