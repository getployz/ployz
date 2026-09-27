// @vitest-environment jsdom
import { orgStoreOptions } from "#/collections/org-store";
import { orgStoreSeed, orgStoreTableNames } from "#/test/org-store-tables";
import { environmentChangeStateOptions } from "#/modules/deployments/environment-change-state.queries";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { Fragment } from "react";
import { getDbClient } from "#/collections/scope";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { DashboardShell } from "./dashboard-shell";
import type { DashboardScope } from "./dashboard-navigation-model";
import { ThemeProvider } from "./theme-provider";
import { authClient } from "#/auth/auth-client";
import type { AuthSession } from "#/auth/auth";
import { Route as RootRoute } from "#/routes/__root";
import { organizationKeys } from "#/modules/environment-design/workspace.queries";

// Better Auth captures fetch when the client is created.
const transport = vi.hoisted(() => {
  const fetch = vi.fn();
  vi.stubGlobal("fetch", fetch);
  return fetch;
});

const clients: QueryClient[] = [];
const scrollSelector = '[data-scroll-restoration-id="wireframe-content"]';
const originalScrollTo = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollTo");
const testSession: NonNullable<AuthSession> = {
  session: { id: "test-session", userId: "test-user" },
  user: { id: "test-user", name: "Test User", email: "test@example.com" },
};
let savedSession: NonNullable<AuthSession>;
beforeEach(() => {
  savedSession = structuredClone(testSession);
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: false, media: query, addEventListener() {}, removeEventListener() {} }));
  transport.mockImplementation(async () => Response.json(savedSession));
  const session = authClient.$store.atoms["session"];
  if (!session) throw new Error("Auth session store unavailable");
  session.set({ ...session.get(), data: savedSession, error: null, isPending: false, isRefetching: false });
  vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} });
  vi.stubGlobal("scrollTo", () => {});
  Object.defineProperty(HTMLElement.prototype, "scrollTo", {
    configurable: true,
    value({ top = 0, left = 0 }: ScrollToOptions) { this.scrollTop = top; this.scrollLeft = left; },
  });
});
afterEach(() => {
  cleanup();
  document.cookie = "theme=; path=/; max-age=0";
  document.documentElement.classList.remove("light", "dark", "system");
  document.documentElement.style.removeProperty("color-scheme");
  for (const client of clients.splice(0)) client.clear();
  vi.clearAllMocks(); vi.unstubAllGlobals();
  if (originalScrollTo) Object.defineProperty(HTMLElement.prototype, "scrollTo", originalScrollTo);
  else Reflect.deleteProperty(HTMLElement.prototype, "scrollTo");
});

async function show({ orgStore = "ready", scope = "environment", billingEnabled = false }: {
  orgStore?: "ready" | "pending" | "failed";
  scope?: DashboardScope["kind"];
  billingEnabled?: boolean;
} = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  clients.push(client);
  const environmentData = { intent: { version: 1, environmentSlug: "production", services: [], volumes: [] }, createdAt: new Date(0), id: "production", projectId: "project", namespace: "production", name: "Production" };
  client.setQueryData(organizationKeys.state("acme"), {
    activeOrganization: { id: "org", slug: "acme", name: "Acme" },
    organizations: [{ id: "org", slug: "acme", name: "Acme" }, { id: "other", slug: "other", name: "Other" }],
    billingEnabled,
  });
  client.setQueryData(["collections", "test-session", "test-user", "acme", "project"], orgStoreSeed([{ id: "project", slug: "store", name: "Store", resolvedEnvironment: environmentData }]));
  client.setQueryData(["collections", "test-session", "test-user", "acme", "environment_summary"], orgStoreSeed([environmentData]));
  client.setQueryData(["collections", "test-session", "test-user", "acme", "project_preference"], orgStoreSeed([]));
  for (const table of orgStoreTableNames.filter((name) => !["project", "environment_summary", "project_preference"].includes(name))) {
    client.setQueryData(["collections", "test-session", "test-user", "acme", table], orgStoreSeed(table === "environment" ? [environmentData] : []));
  }
  const storeScope = { queryClient: client, sessionId: "test-session", userId: "test-user" };
  client.setQueryData(environmentChangeStateOptions("acme", storeScope).queryKey, []);
  const store = orgStoreOptions("acme", storeScope);
  let resolveOrgStore = (_ready: boolean) => {};
  if (orgStore === "ready") client.setQueryData(store.queryKey, true);
  else if (orgStore === "pending") void client.fetchQuery({ ...store, queryFn: () => new Promise<boolean>((resolve) => { resolveOrgStore = resolve; }) });
  else await client.fetchQuery({ ...store, queryFn: () => Promise.reject(new Error("offline")) }).catch(() => {});
  RootRoute.updateLoader({ loader: () => ({ theme: "light", session: savedSession }) });
  Object.assign(RootRoute.options, { shellComponent: Fragment });
  const root = RootRoute.update({ component: () => <ThemeProvider theme="light"><Outlet /></ThemeProvider> });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: savedSession }) });
  const cloud = createRoute({ getParentRoute: () => protectedRoute, path: "cloud" });
  const organization = createRoute({ getParentRoute: () => cloud, path: "$organizationSlug", component: () => (
    <DashboardShell scope={scope === "all"
      ? { kind: "all", organizationSlug: "acme" }
      : { kind: "environment", organizationSlug: "acme", projectSlug: "store", environmentSlug: "production" }}><Outlet /></DashboardShell>
  ) });
  const projectLayout = createRoute({ getParentRoute: () => organization, id: "_project" });
  const project = createRoute({ getParentRoute: () => projectLayout, path: "$projectSlug" });
  const environment = createRoute({ getParentRoute: () => project, path: "$environmentSlug" });
  const logs = createRoute({ getParentRoute: () => environment, path: "logs", component: () => <div>Log entries</div> });
  const settings = createRoute({ getParentRoute: () => environment, path: "settings", component: () => <div>Environment preferences</div> });
  const router = createRouter({
    context: { queryClient: client, dbClient: getDbClient(client) },
    routeTree: root.addChildren([protectedRoute.addChildren([cloud.addChildren([organization.addChildren([projectLayout.addChildren([project.addChildren([environment.addChildren([logs, settings])])])])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/store/production/logs"] }),
    scrollRestoration: true, scrollToTopSelectors: [scrollSelector],
  });
  await router.load();
  const view = render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
  const rail = await screen.findByRole("navigation", { name: "Dashboard navigation" });
  if (orgStore === "ready" && scope === "environment") {
    await waitFor(() => expect(screen.getAllByRole("button", { name: "Environment: Production" }).length).toBeGreaterThan(0));
  }
  return { ...view, router, rail, resolveOrgStore };
}

async function openAccountMenu(rail: HTMLElement) {
  fireEvent.click(within(rail).getByRole("button", { name: "Open account menu" }));
  return screen.findByRole("menu");
}

it("keeps the shell usable while the Org Store loads, then reveals the page", async () => {
  const { resolveOrgStore } = await show({ orgStore: "pending" });
  expect(await screen.findByRole("status", { name: "Loading page" })).toBeTruthy();
  expect(screen.queryByText("Log entries")).toBeNull();
  await act(async () => { resolveOrgStore(true); });
  expect(await screen.findByText("Log entries")).toBeTruthy();
});

it("shows a retryable Org Store failure inside the shell and recovers", async () => {
  await show({ orgStore: "failed" });
  expect(await screen.findByText("Organization data couldn’t load")).toBeTruthy();
  expect(screen.getByRole("navigation", { name: "Dashboard navigation" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  expect(await screen.findByText("Log entries")).toBeTruthy();
  expect(screen.queryByText("Organization data couldn’t load")).toBeNull();
});

it("puts the logo, the four places and the avatar in the rail, with the current place marked", async () => {
  const { rail } = await show();
  const links = within(rail).getAllByRole("link");
  expect(links.map((link) => link.getAttribute("aria-label") ?? link.textContent)).toEqual(["Projects", "Canvas", "Deployments", "Logs", "Settings"]);
  expect(links[0]?.getAttribute("href")).toBe("/cloud/acme/~");
  expect(links[0]?.getAttribute("title")).toBe("Projects");
  expect(links.filter((link) => link.getAttribute("aria-current") === "page").map((link) => link.textContent)).toEqual(["Logs"]);
  expect(within(rail).getByRole("button", { name: "Open account menu" }).getAttribute("title")).toBe("Test User");
  // The top bar names the place after the crumbs; no second title row.
  const banner = screen.getByRole("banner");
  expect(within(banner).getByText("Logs")).toBeTruthy();
  expect(screen.queryByRole("heading")).toBeNull();
});

it("offers the same places in the phone tab bar and follows the route", async () => {
  const { router } = await show();
  const tabs = screen.getByRole("navigation", { name: "Environment places" });
  expect(within(tabs).getAllByRole("link").map((link) => link.textContent)).toEqual(["Canvas", "Deployments", "Logs", "Settings"]);
  fireEvent.click(within(tabs).getByRole("link", { name: "Settings" }));
  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/store/production/settings"));
  await waitFor(() => expect(within(tabs).getByRole("link", { name: "Settings" }).getAttribute("aria-current")).toBe("page"));
  expect(within(tabs).getByRole("link", { name: "Logs" }).getAttribute("aria-current")).toBeNull();
});

it("holds the organization destinations in the avatar menu, and applies a theme choice", async () => {
  const { rail } = await show();
  const menu = await openAccountMenu(rail);
  expect(within(menu).getAllByRole("menuitem").map((item) => item.textContent)).toEqual(
    ["Projects", "Servers", "Server Settings", "Switch organization", "Log out"],
  );
  expect(within(menu).getByRole("menuitem", { name: "Servers" }).getAttribute("href")).toBe("/cloud/acme/~/servers");
  expect(within(menu).getAllByRole("menuitemradio").map((item) => item.textContent)).toEqual(["System", "Light", "Dark"]);
  expect(screen.getAllByText("test@example.com").length).toBeGreaterThan(0);
  fireEvent.click(within(menu).getByRole("menuitemradio", { name: "Dark" }));
  expect(document.documentElement.classList.contains("dark")).toBe(true);
  expect(document.cookie).toContain("theme=dark");
});

it("shows Billing in the avatar menu only when billing is configured", async () => {
  const { rail } = await show({ billingEnabled: true });
  const menu = await openAccountMenu(rail);
  expect(within(menu).getByRole("menuitem", { name: "Billing" }).getAttribute("href")).toBe("/cloud/acme/~/billing");
});

it("shows only the logo and the avatar in the rail on organization pages", async () => {
  const { rail } = await show({ scope: "all" });
  expect(within(rail).getAllByRole("link").map((link) => link.getAttribute("aria-label"))).toEqual(["Projects"]);
  expect(within(rail).getAllByRole("button").map((button) => button.getAttribute("aria-label"))).toEqual(["Open account menu"]);
  expect(screen.queryByRole("navigation", { name: "Environment places" })).toBeNull();
});

it("resets its persistent scroll surface when navigating to a different environment page", async () => {
  const { container, router } = await show();
  const surface = container.querySelector<HTMLElement>(scrollSelector);
  expect(surface).not.toBeNull();
  if (!surface) throw new Error("Dashboard scroll surface not found");
  surface.scrollTop = 640; fireEvent.scroll(surface);
  await act(async () => { router.history.push("/cloud/acme/store/production/settings"); });
  await screen.findByText("Environment preferences");
  expect(container.querySelector(scrollSelector)).toBe(surface);
  await waitFor(() => expect(surface.scrollTop).toBe(0));
});
