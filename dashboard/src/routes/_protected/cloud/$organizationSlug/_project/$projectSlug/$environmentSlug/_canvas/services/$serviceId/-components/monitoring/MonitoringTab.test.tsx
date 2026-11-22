// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import {
  createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider,
} from "@tanstack/react-router";
import type { JsonValue, MachineId, ServiceListing, ServiceSettingChange } from "@ployz/sdk";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { needsProduction, type NeedsProduction } from "#/modules/billing/billing";
import { storeDeploymentsOptions } from "#/modules/config-store/store-view.queries";
import * as servers from "#/modules/machines/use-servers";
import type { Gap, MetricRead, ServiceHistory, ServiceOnServer, ServicePoint } from "#/modules/metrics/metrics";
import { rangeLabel } from "#/lib/time-range";
import type { IncidentObservation, ServiceUptime } from "#/modules/uptime/uptime-status";
import { MonitoringTab } from "./MonitoringTab";

beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }));
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

const now = Date.now();
const openedAt = new Date(now - 12 * 60_000).toISOString();
const failing = (status: number): IncidentObservation =>
  ({ region: "ams", kind: "confirm", verdict: "fail", status, failedPhase: null, error: null, remoteIp: null, at: openedAt });
const uptime = (patch: Partial<ServiceUptime> = {}): ServiceUptime => ({
  at: now,
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
const sampled = [{ at: now - 60_000, cpu: 1 }];
const answered = (machine: MachineId, gaps: readonly Gap[] = []): ServiceOnServer => ({ machine, points: sampled, gaps, markers: [] });
const metrics = (history: Partial<ServiceHistory> = {}): MetricRead<ServiceHistory> => ({
  type: "history",
  window: { from: now - 3_600_000, to: now },
  at: now,
  history: { stepMs: 60_000, points: [], servers: [], ...history },
});

const range = { from: "1h", to: "now" };
const store = { project: "shop", environment: "production" };

const settled = () => screen.findAllByText(/^(Disk|Metrics aren’t enabled on this Service|Not reporting)$/);

async function open({ shown = uptime(), change, checker = true, read = metrics(), placedOn = 1, listed = "ready", until = settled, receivedAt = now }: {
  shown?: ServiceUptime | NeedsProduction | null;
  change?: ServiceSettingChange;
  checker?: boolean;
  read?: MetricRead<ServiceHistory>;
  placedOn?: number;
  listed?: "ready" | "loading";
  until?: () => Promise<HTMLElement[]>;
  receivedAt?: number;
} = {}) {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: Infinity } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  vi.spyOn(servers, "useServerList").mockReturnValue({
    state: listed,
    servers: listed === "loading" ? [] : [{ machine: { id: WEB_1 }, name: "web-1" }, { machine: { id: WEB_2 }, name: "web-2" }],
  } as never);
  queryClient.setQueryData(["uptime", "env", "api", range.from, range.to], shown, { updatedAt: receivedAt });
  queryClient.setQueryData(["metrics", "env", "api", range.from, range.to], read, { updatedAt: receivedAt });
  queryClient.setQueryData(storeDeploymentsOptions("acme", scope, store).queryKey,
    { pages: [{ view: "deployments", deployments: [], next_cursor: null }], pageParams: [null] } as never);
  const set = vi.fn((_: JsonValue | null) => Promise.resolve());

  const root = createRootRoute({ component: Outlet, loader: () => ({ timeZone: "UTC" }) });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", component: Outlet });
  const organization = createRoute({ getParentRoute: () => protectedRoute, path: "cloud/$organizationSlug", component: Outlet });
  const projectGroup = createRoute({ getParentRoute: () => organization, id: "_project", component: Outlet });
  const environment = createRoute({
    getParentRoute: () => projectGroup, path: "$projectSlug/$environmentSlug",
    loader: () => ({ environmentId: "env", store }),
    component: () => (
      <MonitoringTab organizationSlug="acme" checksUptime={checker} service={{ id: "api", name: "api" } as ServiceListing} range={range}
        onRange={() => {}} vcpuLimit={null} volume={null} placedOn={placedOn} healthcheck={{ value: { path: "/health", timeoutSeconds: 30 }, change, set }} />
    ),
  });
  const router = createRouter({
    routeTree: root.addChildren([protectedRoute.addChildren([organization.addChildren([projectGroup.addChildren([environment])])])]),
    history: createMemoryHistory({ initialEntries: ["/cloud/acme/shop/production"] }),
  });
  render(<QueryClientProvider client={queryClient}><RouterProvider router={router} /></QueryClientProvider>);
  await until();
  return { set, queryClient };
}

const statusLine = () => screen.getByRole("heading", { level: 2 }).textContent;
const evidence = () => [...document.querySelectorAll("[data-slot=field-description]")].map((node) => node.textContent);
const charts = () => screen.queryAllByRole("heading", { level: 3 }).map((heading) => heading.textContent);

it("keeps the checked URL in the status line while an incident is open", async () => {
  await open({ shown: down(502, 0.5) });
  expect(statusLine()).toBe("Unreachable· 12m ·api.acme.dev/health→ 502");
});

it("measures an open incident from when the server read, not from when the browser received the read", async () => {
  await open({ shown: down(502, 0.5), receivedAt: now + 5 * 60_000 });
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

it("offers Production in place of the uptime parts, and keeps every resource chart", async () => {
  const { queryClient } = await open({ shown: needsProduction });
  expect(uptimeShown()).toEqual({ status: false, history: false, responseTime: false, checkNow: false });
  expect(charts()).toEqual(["CPU", "Memory", "Network", "Disk"]);
  expect(screen.getByText("Uptime is part of Production")).toBeDefined();
  expect(screen.getByRole("button", { name: "See Production" }).getAttribute("href")).toBe("/cloud/acme/~/billing");
  expect(queryClient.getQueryCache().find({ queryKey: ["uptime", "env", "api", range.from, range.to] })?.state.data).toEqual(needsProduction);
});

it("shows no upsell for a Service that has no uptime check", async () => {
  await open({ shown: null });
  expect(screen.queryByText("Uptime is part of Production")).toBeNull();
});

it("says No readings on each chart, not a zero, when no Server has sampled the Service", async () => {
  await open({ checker: false });
  expect(screen.getAllByText("No readings")).toHaveLength(4);
});

it("replaces the charts with the off state when Cloud keeps no metrics", async () => {
  await open({ checker: false, read: { type: "not_configured", window: { from: now - 3_600_000, to: now }, at: now } });
  expect(charts()).toEqual([]);
  expect(screen.getByRole("status").textContent).toContain("Metrics aren’t enabled on this Service");
  expect(screen.getByRole("link", { name: "Set up metrics" })).toBeTruthy();
});

it("replaces the charts with the off state when the Service's Server sends nothing", async () => {
  await open({ checker: false, read: { type: "not_reporting", window: { from: now - 3_600_000, to: now }, at: now } });
  expect(charts()).toEqual([]);
  expect(screen.getByRole("status").textContent).toContain("Not reporting");
});

it("offers All and By server when the Service runs on several Servers and one has readings", async () => {
  const oneAnswered = metrics({ points: sampled, servers: [answered(WEB_1)] });
  await open({ checker: false, read: oneAnswered, placedOn: 2 });
  expect(screen.getByRole("button", { name: "By server" })).toBeDefined();

  cleanup();
  await open({ checker: false, read: oneAnswered, placedOn: 1 });
  expect(screen.queryByRole("button", { name: "By server" })).toBeNull();

  cleanup();
  await open({ checker: false, read: metrics(), placedOn: 2 });
  expect(screen.queryByRole("button", { name: "By server" })).toBeNull();
});

it("hatches only where no Server has data, in All and By server alike", async () => {
  const gap = { from: now - 3_000_000, to: now - 600_000 };
  await open({ checker: false, read: metrics({ points: sampled, servers: [answered(WEB_1), answered(WEB_2, [gap])] }), placedOn: 2 });
  expect(screen.queryAllByText("Not measured")).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "By server" }));
  expect(screen.queryAllByText("Not measured")).toHaveLength(0);

  cleanup();
  await open({ checker: false, read: metrics({ points: sampled, servers: [answered(WEB_1, [gap]), answered(WEB_2, [gap])] }), placedOn: 2 });
  expect(screen.getAllByText("Not measured")).toHaveLength(4);
  fireEvent.click(screen.getByRole("button", { name: "By server" }));
  expect(screen.getAllByText("Not measured")).toHaveLength(4);
});

it("shows the memory limit only when the newest memory reading carries it", async () => {
  const GIB = 1024 ** 3;
  const memory = (points: ServicePoint[]) => metrics({ points, servers: [{ machine: WEB_1, points, gaps: [], markers: [] }] });
  await open({ checker: false, read: memory([
    { at: now - 1_200_000, memory: GIB / 2, memoryLimit: GIB },
    { at: now - 60_000, memory: GIB / 2 },
  ]) });
  expect(screen.queryByText(/^Limit/)).toBeNull();

  cleanup();
  await open({ checker: false, read: memory([{ at: now - 60_000, memory: GIB / 2, memoryLimit: GIB }]) });
  expect(screen.getByText("Limit 1 GB")).toBeDefined();
});
