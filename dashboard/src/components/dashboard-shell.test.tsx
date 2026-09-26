// @vitest-environment jsdom
import { orgStoreOptions } from "#/collections/org-store";
import { orgStoreSeed, orgStoreTableNames } from "#/test/org-store-tables";
import { environmentChangeStateOptions } from "#/modules/deployments/environment-change-state.queries";
import { act, cleanup, fireEvent, render, screen, waitFor, within, type RenderOptions } from "@testing-library/react";
import { Schema } from "effect";
import { Fragment } from "react";
import { getDbClient } from "#/collections/scope";
import { renderToString } from "react-dom/server";
import { hydrate } from "@tanstack/react-router/ssr/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRoute, createRouter, Outlet, RouterProvider } from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { SidebarTrigger, useSidebar } from "./ui/sidebar";
import { DashboardSidebarProvider, DashboardShell } from "./dashboard-shell";
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
const mediaListeners = new Map<(event: MediaQueryListEvent) => void, string>();
const scrollSelector = '[data-scroll-restoration-id="wireframe-content"]';
const originalScrollTo = Object.getOwnPropertyDescriptor(HTMLElement.prototype, "scrollTo");
const testSession: NonNullable<AuthSession> = {
  session: { id: "test-session", userId: "test-user" },
  user: { id: "test-user", name: "Test User", email: "test@example.com" },
};
let savedSession: NonNullable<AuthSession>;
beforeEach(() => {
  savedSession = structuredClone(testSession);
  vi.stubGlobal("innerWidth", 1200);
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: query.includes("max-width") && window.innerWidth <= 860,
    addEventListener(_type: string, listener: (event: MediaQueryListEvent) => void) { mediaListeners.set(listener, query); },
    removeEventListener(_type: string, listener: (event: MediaQueryListEvent) => void) { mediaListeners.delete(listener); },
  }));
  transport.mockImplementation(async (input: RequestInfo | URL, init?: RequestInit) => {
    if ((input instanceof Request ? input.url : String(input)).includes("/update-session")) {
      const body = Schema.decodeUnknownSync(Schema.Struct({ sidebarOpen: Schema.Boolean }))(
        input instanceof Request ? await input.json() : JSON.parse(String(init?.body)),
      );
      savedSession = { ...savedSession, session: { ...savedSession.session, sidebarOpen: body.sidebarOpen } };
      return Response.json({ session: savedSession.session });
    }
    return Response.json(savedSession);
  });
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
  document.cookie = "sidebar_state=; path=/; max-age=0";
  document.cookie = "theme=; path=/; max-age=0";
  document.documentElement.classList.remove("light", "dark", "system");
  document.documentElement.style.removeProperty("color-scheme");
  for (const client of clients.splice(0)) client.clear();
  mediaListeners.clear(); vi.clearAllMocks(); vi.unstubAllGlobals();
  if (originalScrollTo) Object.defineProperty(HTMLElement.prototype, "scrollTo", originalScrollTo);
  else Reflect.deleteProperty(HTMLElement.prototype, "scrollTo");
});

function PreferenceProbe() {
  const { open } = useSidebar();
  return <><h1>Logs</h1><SidebarTrigger aria-label={open ? "Collapse sidebar" : "Expand sidebar"} /></>;
}

async function show(ssr = false, orgStore: "ready" | "pending" | "failed" = "ready") {
  const client = new QueryClient({ defaultOptions: { queries: { enabled: false, retry: false, staleTime: Infinity } } });
  clients.push(client);
  const environmentData = { intent: { version: 1, environmentSlug: "production", services: [], volumes: [] }, createdAt: new Date(0), id: "production", projectId: "project", namespace: "production", name: "Production" };
  client.setQueryData(organizationKeys.state("acme"), { activeOrganization: { id: "org", slug: "acme", name: "Acme" }, organizations: [{ id: "org", slug: "acme", name: "Acme" }] });
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
  const root = RootRoute.update({
    component: () => ssr ? <Outlet /> : <ThemeProvider theme="light"><Outlet /></ThemeProvider>,
  });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: savedSession }) });
  const cloud = createRoute({ getParentRoute: () => protectedRoute, path: "cloud" });
  const organization = createRoute({ getParentRoute: () => cloud, path: "$organizationSlug", component: () => ssr ? <Outlet /> : (
    <DashboardShell scope={{ kind: "environment", organizationSlug: "acme", projectSlug: "store", environmentSlug: "production" }}><Outlet /></DashboardShell>
  ) });
  const projectLayout = createRoute({ getParentRoute: () => organization, id: "_project" });
  const project = createRoute({ getParentRoute: () => projectLayout, path: "$projectSlug" });
  const environment = createRoute({ getParentRoute: () => project, path: "$environmentSlug", component: () => ssr ? <DashboardSidebarProvider><PreferenceProbe /></DashboardSidebarProvider> : <Outlet /> });
  const logs = createRoute({ getParentRoute: () => environment, path: "logs", component: () => <div>Log entries</div> });
  const settings = createRoute({ getParentRoute: () => environment, path: "settings", component: () => <div>Environment preferences</div> });
  const create = (isServer: boolean) => createRouter({
    isServer,
    context: { queryClient: client, dbClient: getDbClient(client) },
    routeTree: root.addChildren([protectedRoute.addChildren([cloud.addChildren([organization.addChildren([projectLayout.addChildren([project.addChildren([environment.addChildren([logs, settings])])])])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/store/production/logs"] }),
    scrollRestoration: true, scrollToTopSelectors: [scrollSelector],
  });
  let router = create(ssr);
  await router.load();
  let markup: string | undefined;
  let container: HTMLElement | undefined;
  const onRecoverableError = vi.fn();
  if (ssr) {
    markup = renderToString(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>);
    vi.stubGlobal("$_TSR", {
      buffer: [], h() {}, e() {}, c() {}, p() {},
      router: { manifest: undefined, matches: router.state.matches.map(match => ({
        i: match.id.replaceAll("/", "\0"), u: match.updatedAt, s: match.status, ssr: match.ssr, l: match.loaderData,
        b: match.routeId === "/_protected" ? { session: savedSession } : undefined,
      })) },
    });
    router = create(false);
    await hydrate(router);
    container = document.createElement("div");
    container.innerHTML = markup;
    // The inline TanStack bootstrap self-removes in a browser; hydration state is supplied above.
    container.querySelectorAll("script").forEach(script => script.remove());
    document.body.append(container);
  }
  const options: RenderOptions = { onRecoverableError };
  if (container) { options.container = container; options.hydrate = true; }
  const view = render(<QueryClientProvider client={client}><RouterProvider router={router} /></QueryClientProvider>, options);
  if (orgStore !== "ready") return { ...view, router, markup, onRecoverableError, resolveOrgStore };
  await screen.findByRole("heading", { name: "Logs" });
  if (!ssr) await waitFor(() => expect(screen.getAllByRole("button", { name: "Project and environment: Store / Production" }).length).toBeGreaterThan(0));
  return { ...view, router, markup, onRecoverableError, resolveOrgStore };
}

it("keeps the shell usable while the Org Store loads, then reveals the page", async () => {
  const { resolveOrgStore } = await show(false, "pending");
  expect(await screen.findByRole("status", { name: "Loading page" })).toBeTruthy();
  expect(screen.getByRole("complementary", { name: "Dashboard navigation" })).toBeTruthy();
  expect(screen.queryByText("Log entries")).toBeNull();
  await act(async () => { resolveOrgStore(true); });
  expect(await screen.findByText("Log entries")).toBeTruthy();
});

it("shows a retryable Org Store failure inside the shell and recovers", async () => {
  await show(false, "failed");
  expect(await screen.findByText("Organization data couldn’t load")).toBeTruthy();
  expect(screen.getByRole("complementary", { name: "Dashboard navigation" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  expect(await screen.findByText("Log entries")).toBeTruthy();
  expect(screen.queryByText("Organization data couldn’t load")).toBeNull();
});

it("renders real scope queries and retains named navigation when collapsed", async () => {
  const { router } = await show();
  const sidebar = screen.getByRole("complementary", { name: "Dashboard navigation" });
  expect(within(sidebar).getByRole("button", { name: "Organization: Acme" })).toBeTruthy();
  fireEvent.click(within(sidebar).getByRole("button", { name: "Collapse sidebar" }));
  await within(sidebar).findByRole("button", { name: "Expand sidebar" });
  expect(within(sidebar).getByRole("button", { name: "Expand sidebar" })).toBeTruthy();
  expect(within(sidebar).queryByRole("link", { name: "Ployz home" })).toBeNull();
  expect(within(sidebar).getByRole("link", { name: "Architecture" })).toBeTruthy();
  expect(within(sidebar).getByRole("link", { name: "Logs" }).getAttribute("aria-current")).toBe("page");
  expect(router.state.location.pathname).toBe("/cloud/acme/store/production/logs");
  fireEvent.click(within(sidebar).getByRole("button", { name: "Expand sidebar" }));
  await within(sidebar).findByRole("button", { name: "Collapse sidebar" });
  expect(within(sidebar).getByRole("link", { name: "Ployz home" })).toBeTruthy();
});

it("opens the collapsed account menu on hover and applies a theme choice", async () => {
  await show();
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  await screen.findByRole("button", { name: "Expand sidebar" });
  const account = within(screen.getByRole("complementary", { name: "Dashboard navigation" })).getByRole("button", { name: "Open account menu" });
  fireEvent.mouseEnter(account);
  fireEvent.mouseMove(account);
  const dark = await screen.findByRole("menuitem", { name: "Dark" });
  expect(screen.getAllByText("test@example.com").length).toBeGreaterThan(0);
  fireEvent.click(dark);
  expect(document.documentElement.classList.contains("dark")).toBe(true);
  expect(document.cookie).toContain("theme=dark");
  await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
});

it("restores both sidebar preferences after remounting the dashboard", async () => {
  const first = await show();
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  await screen.findByRole("button", { name: "Expand sidebar" });
  await waitFor(() => expect(savedSession.session.sidebarOpen).toBe(false));
  await waitFor(() => expect(authClient.$store.atoms["session"]?.get().data?.session.sidebarOpen).toBe(false));
  first.unmount();

  const second = await show();
  fireEvent.click(screen.getByRole("button", { name: "Expand sidebar" }));
  await waitFor(() => expect(savedSession.session.sidebarOpen).toBe(true));
  await waitFor(() => expect(authClient.$store.atoms["session"]?.get().data?.session.sidebarOpen).toBe(true));
  second.unmount();

  await show();
  expect(screen.getByRole("button", { name: "Collapse sidebar" })).toBeTruthy();
});

it.each([
  [false, "Expand sidebar"],
  [true, "Collapse sidebar"],
] as const)("reads session sidebarOpen=%s on a fresh dashboard mount", async (saved, action) => {
  savedSession = { ...savedSession, session: { ...savedSession.session, sidebarOpen: saved } };
  const session = authClient.$store.atoms["session"];
  if (!session) throw new Error("Missing session");
  session.set({ ...session.get(), data: savedSession });
  await show();
  expect(screen.getByRole("button", { name: action })).toBeTruthy();
});

it("resets its persistent scroll surface when navigating to a different environment page", async () => {
  const { container, router } = await show();
  const surface = container.querySelector<HTMLElement>(scrollSelector);
  expect(surface).not.toBeNull();
  if (!surface) throw new Error("Dashboard scroll surface not found");
  surface.scrollTop = 640; fireEvent.scroll(surface);
  await act(async () => { router.history.push("/cloud/acme/store/production/settings"); });
  await screen.findByRole("heading", { name: "Settings" });
  expect(container.querySelector(scrollSelector)).toBe(surface);
  await waitFor(() => expect(surface.scrollTop).toBe(0));
});

it("opens the directory from a sibling page without navigating away", async () => {
  const { router } = await show();
  const sidebar = screen.getByRole("complementary", { name: "Dashboard navigation" });
  fireEvent.click(within(sidebar).getByRole("button", { name: "Expand Architecture" }));
  expect(await within(sidebar).findByText("No resources yet")).toBeTruthy();
  expect(router.state.location.pathname).toBe("/cloud/acme/store/production/logs");
  fireEvent.click(within(sidebar).getByRole("button", { name: "Collapse Architecture" }));
  await waitFor(() => expect(within(sidebar).queryByText("No resources yet")).toBeNull());
});

it("uses the mobile picker for the same scoped destinations and closes it after selection", async () => {
  vi.stubGlobal("innerWidth", 390);
  const { router } = await show();
  const main = screen.getByRole("main");
  expect(within(main).getByRole("button", { name: "Organization: Acme" })).toBeTruthy();
  fireEvent.click(within(main).getByRole("button", { name: "Project navigation" }));
  const picker = await screen.findByRole("dialog", { name: "Project navigation" });
  const settings = within(picker).getByRole("link", { name: "Settings" });
  expect(settings.getAttribute("href")).toBe("/cloud/acme/store/production/settings");
  fireEvent.click(settings);
  await waitFor(() => expect(router.state.location.pathname).toBe("/cloud/acme/store/production/settings"));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Project navigation" })).toBeNull());
  expect(within(main).getByRole("button", { name: "Project navigation" }).textContent).toContain("Settings");
});

it("removes an open collapsed-rail popup when resizing to mobile", async () => {
  const { router } = await show();
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  await screen.findByRole("button", { name: "Expand sidebar" });
  fireEvent.mouseEnter(screen.getByRole("link", { name: "Architecture" }));
  fireEvent.mouseMove(screen.getByRole("link", { name: "Architecture" }));
  expect(await screen.findByRole("dialog", { name: "Architecture" })).toBeTruthy();
  await act(async () => {
    vi.stubGlobal("innerWidth", 390);
    for (const [listener, query] of mediaListeners) {
      listener(Object.assign(new Event("change"), { matches: query.includes("max-width"), media: query }));
    }
  });
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Architecture" })).toBeNull());
  expect(screen.queryByRole("complementary", { name: "Dashboard navigation" })).toBeNull();
  expect(screen.getByRole("button", { name: "Project navigation" })).toBeTruthy();
  expect(router.state.location.pathname).toBe("/cloud/acme/store/production/logs");
});

it("renders and hydrates the saved session preference with SSR enabled", async () => {
  savedSession = { ...savedSession, session: { ...savedSession.session, sidebarOpen: false } };
  document.cookie = "sidebar_state=true; path=/";
  const session = authClient.$store.atoms["session"];
  if (!session) throw new Error("Missing session");
  session.set({ ...session.get(), data: savedSession });
  const { markup, onRecoverableError } = await show(true);
  expect(markup).toContain('aria-label="Expand sidebar"');
  expect(markup).not.toContain('aria-label="Collapse sidebar"');
  expect(screen.getByRole("button", { name: "Expand sidebar" })).toBeTruthy();
  expect(onRecoverableError).not.toHaveBeenCalled();
});

it("restores the session value if saving the preference fails", async () => {
  await show();
  const original = transport.getMockImplementation();
  if (!original) throw new Error("Missing transport");
  transport.mockImplementation((input: RequestInfo | URL, init?: RequestInit) =>
    String(input).includes("/update-session")
      ? Promise.resolve(Response.json({ message: "Unavailable" }, { status: 503 }))
      : original(input, init));
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  await waitFor(() => expect(clients.at(-1)?.getMutationCache().getAll().at(-1)?.state.status).toBe("error"));
  expect(screen.getByRole("button", { name: "Collapse sidebar" })).toBeTruthy();
  expect(savedSession.session.sidebarOpen).toBeUndefined();
});

it("reports a failed session refresh after the preference was saved", async () => {
  await show();
  const original = transport.getMockImplementation();
  if (!original) throw new Error("Missing transport");
  transport.mockImplementation((input: RequestInfo | URL, init?: RequestInit) =>
    savedSession.session.sidebarOpen === false
      ? Promise.resolve(Response.json({ message: "Unavailable" }, { status: 503 }))
      : original(input, init));
  fireEvent.click(screen.getByRole("button", { name: "Collapse sidebar" }));
  await waitFor(() => expect(clients.at(-1)?.getMutationCache().getAll().at(-1)?.state.error?.message)
    .toBe("Sidebar preference saved, but session refresh failed. Reload to restore it."));
  expect(savedSession.session.sidebarOpen).toBe(false);
  expect(screen.getByRole("button", { name: "Collapse sidebar" })).toBeTruthy();
});
