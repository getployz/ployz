// @vitest-environment jsdom
import { orgStoreSeed } from "#/test/org-store-tables";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useLiveQuery } from "@tanstack/react-db";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider, useParams } from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DashboardNavigation, EnvironmentNodeDirectory } from "./dashboard-navigation";
import { SidebarProvider, SidebarTrigger, useSidebar } from "./ui/sidebar";
import type { NavigationNode } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/-components/environment-node-navigation";
import { SERVICE_PAGES } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/service-pages";
import { createApiCollection, reconcileCollection } from "#/collections/query-collection";

const nodes: NavigationNode[] = [
  { id: "api", name: "API", type: "service" },
  { id: "worker", name: "Worker", type: "service" },
  { id: "postgres", name: "Postgres", type: "service" },
  { id: "data", name: "Database storage", type: "volume" },
  { id: "uploads", name: "Uploads storage", type: "volume" },
  { id: "scheduler", name: "Scheduler", type: "service" },
  { id: "web", name: "A very long website service name", type: "service" },
];
const clients: QueryClient[] = [];
const scope = { kind: "environment", organizationSlug: "acme", projectSlug: "store", environmentSlug: "staging" } as const;

beforeEach(() => {
  vi.stubGlobal("matchMedia", () => ({ matches: false, addEventListener() {}, removeEventListener() {} }));
  vi.stubGlobal("scrollTo", () => {});
});
afterEach(() => {
  cleanup();
  for (const client of clients.splice(0)) client.clear();
  vi.unstubAllGlobals();
});

function NavigationHarness() {
  const { open } = useSidebar();
  return <>
    <SidebarTrigger aria-label={open ? "Collapse sidebar" : "Expand sidebar"} />
    <DashboardNavigation scope={scope} projection={open ? "desktop" : "rail"} />
  </>;
}

async function showNavigation() {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  clients.push(client);
  for (const table of ["project", "environment", "service", "environment_resource", "resource_lineage", "environment_canvas_node_position", "environment_node_config_snapshot", "volume_remove_attempt"]) {
    client.setQueryData(["collections", "test-session", "test-user", "acme", table], orgStoreSeed([]));
  }
  const root = createRootRoute({ component: Outlet });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "test-session" }, user: { id: "test-user" } } }) });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug" });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project" });
  const environment = createRoute({
    getParentRoute: () => projectGroup,
    path: "$projectSlug/$environmentSlug",
    component: () => <SidebarProvider><NavigationHarness /><Outlet /></SidebarProvider>,
  });
  const canvas = createRoute({ getParentRoute: () => environment, id: "_canvas" });
  const index = createRoute({ getParentRoute: () => canvas, path: "/" });
  const service = createRoute({ getParentRoute: () => canvas, path: "services/$serviceId" });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment.addChildren([canvas.addChildren([index, service])])])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/store/staging"] }),
  });
  await router.load();
  render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
  return router;
}

async function show(
  items: NavigationNode[] = nodes,
  selected = "services/api",
  liveNodes?: ReturnType<typeof createApiCollection<NavigationNode>>,
) {
  function Directory() {
    const params = useParams({ strict: false });
    const live = useLiveQuery(() => liveNodes);
    return <SidebarProvider><EnvironmentNodeDirectory
      scope={{ kind: "environment", organizationSlug: "acme", projectSlug: "store", environmentSlug: "staging" }}
      nodes={liveNodes ? live.data ?? [] : items} selectedId={params.serviceId ?? params.resourceId ?? null}
    /></SidebarProvider>;
  }
  const root = createRootRoute({ component: Directory });
  const service = createRoute({ getParentRoute: () => root, path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId" });
  const resource = createRoute({ getParentRoute: () => root, path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/resources/$resourceId" });
  const architecture = createRoute({ getParentRoute: () => root, path: "/cloud/$organizationSlug/$projectSlug/$environmentSlug" });
  const router = createRouter({
    routeTree: root.addChildren([service, resource, architecture]),
    history: createMemoryHistory({ initialEntries: [`/cloud/acme/store/staging/${selected}?tab=variables&stale=old`] }),
  });
  await router.load();
  render(<RouterProvider router={router} />);
  return router;
}

it("shows the selected service pages first without duplicating its resource row", async () => {
  const router = await show();
  expect(screen.getAllByRole("link").slice(1, SERVICE_PAGES.length + 1).map((link) => link.textContent)).toEqual(SERVICE_PAGES.map((page) => page.label));
  expect(screen.getByRole("link", { name: "API" })).toBeTruthy();
  expect(screen.getByRole("link", { name: "Variables" }).getAttribute("aria-current")).toBe("page");
  fireEvent.click(screen.getByRole("link", { name: "Settings" }));
  await waitFor(() => expect(router.state.location.href).toBe("/cloud/acme/store/staging/services/api?tab=settings"));
  expect(screen.getByRole("link", { name: "Settings" }).getAttribute("aria-current")).toBe("page");
  fireEvent.click(screen.getByRole("link", { name: "Worker" }));
  await waitFor(() => expect(router.state.location.href).toBe("/cloud/acme/store/staging/services/worker"));
  expect(screen.getByRole("link", { name: "Worker" })).toBeTruthy();
  expect(screen.getByRole("link", { name: "API" })).toBeTruthy();
});

it("searches other resources without hiding the selected pages, with a clear no-match state", async () => {
  await show();
  const search = screen.getByRole("searchbox", { name: "Find resource" });
  fireEvent.change(search, { target: { value: "  STORAGE  " } });
  expect(screen.getByRole("link", { name: "Database storage" })).toBeTruthy();
  expect(screen.queryByRole("link", { name: "Worker" })).toBeNull();
  expect(screen.getByRole("link", { name: "Variables" })).toBeTruthy();
  fireEvent.change(search, { target: { value: "missing" } });
  expect(screen.getByText("No matching resources")).toBeTruthy();
  fireEvent.change(search, { target: { value: "" } });
  expect(screen.queryByText("No matching resources")).toBeNull();
  expect(screen.getByRole("link", { name: "A very long website service name" }).getAttribute("title")).toBe("A very long website service name");
});

it("keeps an active filter clearable when a live resource removal hides the search threshold", async () => {
  const client = new QueryClient();
  clients.push(client);
  let rows = nodes;
  const collection = createApiCollection({
    queryClient: client,
    queryKey: ["navigation-resources"],
    queryFn: async () => rows,
    getKey: (row: NavigationNode) => row.id,
  });
  await collection.preload();
  try {
    await show(nodes, "services/api", collection);
    const search = await screen.findByRole<HTMLInputElement>("searchbox", { name: "Find resource" });
    fireEvent.change(search, { target: { value: "website" } });
    expect(screen.getByRole("link", { name: "A very long website service name" })).toBeTruthy();
    rows = nodes.slice(0, 6);
    await act(async () => { await reconcileCollection(collection); });
    await waitFor(() => expect(screen.queryByRole("link", { name: "A very long website service name" })).toBeNull());
    expect(screen.getByRole("searchbox", { name: "Find resource" })).toBe(search);
    expect(search.value).toBe("website");
    expect(screen.getByText("No matching resources")).toBeTruthy();
    fireEvent.change(search, { target: { value: "" } });
    expect(screen.getByRole("link", { name: "Worker" })).toBeTruthy();
    expect(screen.queryByText("No matching resources")).toBeNull();
    expect(screen.queryByRole("searchbox", { name: "Find resource" })).toBeNull();
  } finally {
    cleanup();
    await collection.cleanup();
  }
});

it("opens a Volume as a resource and exposes only Configuration", async () => {
  const router = await show();
  const id = "data";
  const node = nodes.find((candidate) => candidate.id === id);
  if (!node) throw new Error(`Missing node fixture: ${id}`);
  fireEvent.click(screen.getByRole("link", { name: node.name }));
  await waitFor(() => expect(router.state.location.href).toBe(`/cloud/acme/store/staging/resources/${id}`));
  expect(screen.getByRole("link", { name: "Settings" }).getAttribute("aria-current")).toBe("page");
  expect(screen.queryByRole("link", { name: "Variables" })).toBeNull();
  expect(screen.getByRole("link", { name: node.name })).toBeTruthy();
});

it("shows a true empty state but omits the other-resources section for a sole selected node", async () => {
  await show([], "");
  expect(screen.getByText("No resources yet")).toBeTruthy();
  expect(screen.queryByRole("searchbox")).toBeNull();
  cleanup();
  await show(nodes.slice(0, 1));
  expect(screen.queryByText("Other resources")).toBeNull();
  expect(screen.queryByText("No other resources")).toBeNull();
  expect(screen.getByRole("link", { name: "Settings" })).toBeTruthy();
  expect(screen.queryByRole("searchbox")).toBeNull();
});

it("does not turn an expanded directory into a popup when collapsing the sidebar", async () => {
  const router = await showNavigation();
  await act(async () => { router.history.push("/cloud/acme/store/staging/services/api"); });
  expect(screen.getByRole("button", { name: "Collapse Architecture" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull();
  fireEvent.mouseEnter(screen.getByRole("link", { name: "Architecture" }));
  fireEvent.mouseMove(screen.getByRole("link", { name: "Architecture" }));
  expect(await screen.findByRole("dialog", { name: "Architecture" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Expand sidebar" }));
  expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull();
});

it("reveals the collapsed Architecture menu on hover without stealing focus", async () => {
  await showNavigation();
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  const expand = screen.getByRole("button", { name: "Expand sidebar" });
  act(() => expand.focus());
  const trigger = screen.getByRole("link", { name: "Architecture" });
  fireEvent.mouseEnter(trigger);
  fireEvent.mouseMove(trigger);
  const menu = await screen.findByRole("dialog", { name: "Architecture" });
  expect(document.activeElement).toBe(expand);
  fireEvent.mouseEnter(menu);
  expect(screen.getAllByRole("link", { name: "Architecture" })).toHaveLength(2);
  fireEvent.keyDown(menu, { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull());
  expect(document.activeElement).toBe(expand);
  fireEvent.click(trigger);
  expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull();
});

it("keeps the rail directory closed on service selection and page changes", async () => {
  const router = await showNavigation();
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  await act(async () => { router.history.push("/cloud/acme/store/staging/services/api"); });
  expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull();
  await act(async () => { router.history.push("/cloud/acme/store/staging/services/api?tab=variables"); });
  expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull();
  await act(async () => { router.history.push("/cloud/acme/store/staging/services/worker"); });
  expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull();
});

it("expands a resource independently without navigating", async () => {
  const router = await show();
  const href = router.state.location.href;
  fireEvent.click(screen.getByRole("button", { name: "Collapse API" }));
  await waitFor(() => expect(screen.queryByRole("link", { name: "Variables" })).toBeNull());
  fireEvent.click(screen.getByRole("button", { name: "Expand Worker" }));
  expect(await screen.findByRole("link", { name: "Variables" })).toBeTruthy();
  expect(router.state.location.href).toBe(href);
  fireEvent.click(screen.getByRole("link", { name: "Variables" }));
  await waitFor(() => expect(router.state.location.href).toBe("/cloud/acme/store/staging/services/worker?tab=variables"));
});

it("navigates to Architecture when the rail icon is clicked", async () => {
  const router = await showNavigation();
  await act(async () => { router.history.push("/cloud/acme/store/staging/services/api"); });
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  fireEvent.click(screen.getByRole("link", { name: "Architecture" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/store/staging"));
  expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull();
});
