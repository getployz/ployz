// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import type { JsonValue, MachineId, MetricHistory, ServiceListing, ServiceSettingChange } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { storeDeploymentsOptions } from "#/modules/config-store/store-view.queries";
import * as servers from "#/modules/machines/use-servers";
import type { MetricRead } from "#/modules/metrics/metrics";
import { rangeLabel } from "#/modules/uptime/uptime-range";
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
  status: "up", uptime: 1, points: [], checks: null, latest: [], incidents: [], regions: {}, probe: null, ...patch,
});
/** An open incident whose first failure answered `status`, over a range the check passed `share` of. */
const down = (status: number, share: number) => uptime({
  status: "unreachable", uptime: share,
  incidents: [{ id: "inc", openedAt, closedAt: null, observations: [failing(status)] }],
});

const WEB_1 = "a".repeat(32) as MachineId;
const WEB_2 = "b".repeat(32) as MachineId;
const nanos = (ms: number) => ms * 1e6;
type Unanswered = MetricHistory["unanswered"][number];
const refused = (machine_id: MachineId): Unanswered =>
  ({ machine_id, error: { code: "unsupported", message: "old", details: { capability: "ployz.metrics.history.v1", target: "x" } } });
const silent = (machine_id: MachineId): Unanswered =>
  ({ machine_id, error: { code: "unavailable", message: "the Server is not up, so it was not asked", details: null } });
const sampled = { service: [{ ts: nanos(now - 60_000), cpu_usec: 1_000_000 }] };
const answered = (machine_id: MachineId) => ({ machine_id, value: { points: sampled, step_nanos: nanos(60_000), gaps: [] } });
const metrics = (history: Partial<MetricHistory> = {}): MetricRead => ({
  type: "history",
  window: { from: now - 3_600_000, to: now },
  history: { step_nanos: nanos(60_000), points: { service: [] }, machines: [], unanswered: [], ...history },
});

const range = { from: "1h", to: "now" };
const store = { project: "shop", environment: "production" };

async function open({ shown = uptime(), change, checker = true, read = metrics() }: {
  shown?: ServiceUptime | null;
  change?: ServiceSettingChange;
  checker?: boolean;
  read?: MetricRead;
} = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  vi.spyOn(servers, "useServerList").mockReturnValue({
    state: "ready",
    servers: [{ machine: { id: WEB_1 }, name: "web-1" }, { machine: { id: WEB_2 }, name: "web-2" }],
  } as never);
  queryClient.setQueryData(["uptime", "env", "api", range.from, range.to], shown, { updatedAt: now });
  queryClient.setQueryData(["metrics", "env", "api", range.from, range.to], read, { updatedAt: now });
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
      <MonitoringTab organizationSlug="acme" checksUptime={checker} service={{ id: "api", name: "api" } as ServiceListing} range={range}
        onRange={() => {}} vcpuLimit={null} volume={null} healthcheck={{ value: { path: "/health", timeoutSeconds: 30 }, change, set }} />
    ),
  });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production"] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  await screen.findAllByRole("heading", { level: 3, name: /^(Disk|Upgrade .*|Your servers are offline)$/ });
  return { set, queryClient };
}

const statusLine = () => screen.getByRole("heading", { level: 2 }).textContent;
const evidence = () => [...document.querySelectorAll("[data-slot=field-description]")].map((node) => node.textContent);
const charts = () => screen.queryAllByRole("heading", { level: 3 }).map((heading) => heading.textContent);

it("keeps the checked URL in the status line while an incident is open", async () => {
  await open({ shown: down(502, 0.5) });
  expect(statusLine()).toBe("Unreachable· 12m ·api.acme.dev/health→ 502");
});

it("puts the Healthcheck path field under the status line when the check never passed and its path answers a 4xx", async () => {
  const { set } = await open({ shown: down(404, 0) });
  expect(evidence()).toEqual(["Your app answers /health with 404."]);
  expect(screen.queryByLabelText("Healthcheck timeout")).toBeNull();

  const path = screen.getByLabelText("Healthcheck path");
  expect(path).toHaveProperty("value", "/health");
  fireEvent.change(path, { target: { value: "/healthz" } });
  fireEvent.keyDown(path, { key: "Enter" });
  expect(set).toHaveBeenCalledExactlyOnceWith({ path: "/healthz", timeoutSeconds: 30 });
});

it("keeps the field, marked changed, while the new path waits for a deploy", async () => {
  await open({ shown: down(404, 0), change: { path: "api.healthcheck", kind: "update", before: { path: "/health", timeoutSeconds: 30 },
    after: { path: "/healthz", timeoutSeconds: 30 }, canRestore: true, row: null } });
  expect(screen.getByLabelText("Healthcheck path").closest("[data-changed=true]")).not.toBeNull();
});

it.each([
  ["a 5xx", down(502, 0)],
  ["a 4xx after the check once passed", down(404, 0.5)],
  ["no incident", uptime()],
])("leaves the field out for %s", async (_, shown) => {
  await open({ shown });
  expect(screen.queryByLabelText("Healthcheck path")).toBeNull();
});

const uptimeShown = () => ({
  status: screen.queryByRole("heading", { level: 2 }) !== null,
  history: screen.queryByLabelText("History") !== null,
  responseTime: screen.queryByRole("heading", { level: 3, name: "Response time" }) !== null,
  checkNow: screen.queryByRole("button", { name: "Check now" }) !== null,
});

it("leads with the status line, history strip and Response time for a checked Service, then every resource chart", async () => {
  await open();
  expect(uptimeShown()).toEqual({ status: true, history: true, responseTime: true, checkNow: true });
  expect(charts()).toEqual(["Response time", "CPU", "Memory", "Network", "Disk"]);
});

it("starts at CPU, with no uptime and no nag, for a Service without a public domain", async () => {
  await open({ shown: null });
  expect(uptimeShown()).toEqual({ status: false, history: false, responseTime: false, checkNow: false });
  expect(charts()).toEqual(["CPU", "Memory", "Network", "Disk"]);
  expect(screen.queryByText(/domain/i)).toBeNull();
});

it("shows no uptime and reads none on a Cloud without the uptime checker, keeping the metrics and their range", async () => {
  const { queryClient } = await open({ checker: false });
  expect(uptimeShown()).toEqual({ status: false, history: false, responseTime: false, checkNow: false });
  expect(charts()).toEqual(["CPU", "Memory", "Network", "Disk"]);
  expect(screen.getByRole("button", { name: rangeLabel(range, "UTC", Date.now()) })).toBeDefined();
  expect(queryClient.getQueryCache().findAll({ queryKey: ["uptime"] }).map((query) => query.getObserversCount())).toEqual([0]);
});

it("says No readings on each chart, not a zero, when no Server has sampled the Service", async () => {
  await open({ checker: false });
  expect(screen.getAllByText("No readings")).toHaveLength(4);
});

it("replaces the charts with a link to the Server when the only Server needs an upgrade", async () => {
  await open({ checker: false, read: metrics({ unanswered: [refused(WEB_2)] }) });
  expect(charts()).toEqual(["Upgrade web-2 to see its metrics"]);
  expect(screen.getByRole("link", { name: "Open web-2" }).getAttribute("href")).toBe(`/cloud/acme/~/servers/${WEB_2}`);
});

it("counts the Servers and links to the list when several need an upgrade", async () => {
  await open({ checker: false, read: metrics({ unanswered: [refused(WEB_1), refused(WEB_2)] }) });
  expect(charts()).toEqual(["Upgrade 2 servers to see their metrics"]);
  expect(screen.getByRole("link", { name: "Open Servers" }).getAttribute("href")).toBe("/cloud/acme/~/servers");
});

it("keeps the charts and names a Server that needs an upgrade or didn't answer when another answered", async () => {
  await open({ checker: false, read: metrics({ points: sampled, machines: [answered(WEB_1)], unanswered: [refused(WEB_2)] }) });
  expect(charts()).toEqual(["CPU", "Memory", "Network", "Disk"]);
  expect(screen.getByText(/needs an upgrade/).textContent).toBe("web-2 needs an upgrade to show its metrics.");

  cleanup();
  await open({ checker: false, read: metrics({ points: sampled, machines: [answered(WEB_1)], unanswered: [silent(WEB_2)] }) });
  expect(screen.getByText(/didn’t answer/).textContent).toBe("web-2 didn’t answer.");
});

it("offers All and By server only when more than one Server answered", async () => {
  await open({ checker: false, read: metrics({ points: sampled, machines: [answered(WEB_1)], unanswered: [refused(WEB_2)] }) });
  expect(screen.queryByRole("button", { name: "By server" })).toBeNull();

  cleanup();
  await open({ checker: false, read: metrics({ points: sampled, machines: [answered(WEB_1), answered(WEB_2)] }) });
  expect(screen.getByRole("button", { name: "By server" })).toBeDefined();
});

it("hatches a silent Server's own gap in By server, where All still has the other Server's line", async () => {
  const gap = { from: nanos(now - 3_000_000), to: nanos(now - 600_000), reason: "timeout" as const };
  const quiet = { machine_id: WEB_2, value: { points: sampled, step_nanos: nanos(60_000), gaps: [gap] } };
  await open({ checker: false, read: metrics({ points: sampled, machines: [answered(WEB_1), quiet] }) });
  expect(screen.queryAllByText("Not measured")).toHaveLength(0);

  fireEvent.click(screen.getByRole("button", { name: "By server" }));
  expect(screen.getAllByText("Not measured")).toHaveLength(4);
});
