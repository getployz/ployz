// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import type { JsonValue, ServiceListing, ServiceSettingChange } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { storeDeploymentsOptions } from "#/modules/config-store/store-view.queries";
import type { IncidentObservation, ServiceUptime } from "#/modules/uptime/uptime-status";
import { MonitoringTab } from "./MonitoringTab";

beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const now = Date.now();
const openedAt = new Date(now - 12 * 60_000).toISOString();
const failing = (status: number): IncidentObservation =>
  ({ region: "ams", kind: "confirm", verdict: "fail", status, failedPhase: null, error: null, remoteIp: null, at: openedAt });
const uptime = (patch: Partial<ServiceUptime> = {}): ServiceUptime => ({
  check: { id: "chk", url: "https://api.acme.dev/health", intervalSeconds: 300 },
  status: "up", uptime: 1, points: [], checks: null, incidents: [], regions: {}, probe: null, ...patch,
});
/** An open incident whose first failure answered `status`, over a range the check passed `share` of. */
const down = (status: number, share: number) => uptime({
  status: "unreachable", uptime: share,
  incidents: [{ id: "inc", openedAt, closedAt: null, observations: [failing(status)] }],
});

const range = { from: "1h", to: "now" };
const store = { project: "shop", environment: "production" };

async function open(shown: ServiceUptime, change?: ServiceSettingChange) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  queryClient.setQueryData(["uptime", "env", "api", range.from, range.to], shown, { updatedAt: now });
  queryClient.setQueryData(storeDeploymentsOptions("acme", scope, store).queryKey,
    { pages: [{ view: "deployments", deployments: [], next_cursor: null }], pageParams: [null] } as never);
  const set = vi.fn((_: JsonValue | null) => ({ isPersisted: { promise: Promise.resolve() } }));

  const root = createRootRoute({ component: Outlet, loader: () => ({ timeZone: "UTC" }) });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({
    getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug",
    loader: () => ({ environmentId: "env", store }),
    component: () => (
      <MonitoringTab organizationSlug="acme" service={{ id: "api", name: "api" } as ServiceListing} range={range}
        onRange={() => {}} addDomain={null} healthcheck={{ value: { path: "/health", timeoutSeconds: 30 }, change, set }} />
    ),
  });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production"] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findByRole("heading", { level: 2 });
  return { set };
}

const statusLine = () => screen.getByRole("heading", { level: 2 }).textContent;
const evidence = () => [...document.querySelectorAll("[data-slot=field-description]")].map((node) => node.textContent);

it("keeps the checked URL in the status line while an incident is open", async () => {
  await open(down(502, 0.5));
  expect(statusLine()).toBe("Unreachable· 12m ·api.acme.dev/health→ 502");
});

it("puts the Healthcheck path field under the status line when the check never passed and its path answers a 4xx", async () => {
  const { set } = await open(down(404, 0));
  expect(evidence()).toEqual(["Your app answers /health with 404."]);
  expect(screen.queryByLabelText("Healthcheck timeout")).toBeNull();

  const path = screen.getByLabelText("Healthcheck path");
  expect(path).toHaveProperty("value", "/health");
  fireEvent.change(path, { target: { value: "/healthz" } });
  fireEvent.keyDown(path, { key: "Enter" });
  expect(set).toHaveBeenCalledExactlyOnceWith({ path: "/healthz", timeoutSeconds: 30 });
});

it("keeps the field, marked changed, while the new path waits for a deploy", async () => {
  await open(down(404, 0), { path: "api.healthcheck", kind: "update", before: { path: "/health", timeoutSeconds: 30 },
    after: { path: "/healthz", timeoutSeconds: 30 }, canRestore: true, row: null });
  expect(screen.getByLabelText("Healthcheck path").closest("[data-changed=true]")).not.toBeNull();
});

it.each([
  ["a 5xx", down(502, 0)],
  ["a 4xx after the check once passed", down(404, 0.5)],
  ["no incident", uptime()],
])("leaves the field out for %s", async (_, shown) => {
  await open(shown);
  expect(screen.queryByLabelText("Healthcheck path")).toBeNull();
});
