// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { DbProvider } from "@tanstack/react-db";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterContextProvider } from "@tanstack/react-router";
import { act, cleanup, render, screen, waitFor } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { getDbClient } from "#/collections/scope";
import { getContainerLogStream, type ContainerLogSelection } from "#/modules/runtime/container-log.stream";
import { getRuntimeCollections } from "#/modules/runtime/runtime.collection";
import { ContainerLogs } from "./container-logs";

const SERVICE = "shop-production/api";
const container = (machineId: string, logDriver: string) => ({
  id: machineId, displayName: "api", machineId, namespace: "shop-production", kind: "service_container", runtime: { state: "running" }, logDriver,
});

async function withViewer(selection: ContainerLogSelection, run: (services: ReturnType<typeof getRuntimeCollections>["services"]) => Promise<void>) {
  const sources: EventTarget[] = [];
  class FakeEventSource extends EventTarget {
    constructor() { super(); sources.push(this); }
    close() {}
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({ x: 0, y: 0, top: 0, left: 0, right: 800, bottom: 2_000, width: 800, height: 2_000, toJSON: () => ({}) });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(2_000);
  vi.stubGlobal("fetch", async (url: string) => url.includes("log-history-status")
    ? Response.json({ history: true, now: 900_000, machines: [{ machineId: "quiet", seenAt: 300_000 }] })
    : Response.json({ rows: [], failures: [], cursor: null }));
  const client = new QueryClient();
  const root = createRootRoute({ loader: () => ({ timeZone: "UTC" }) });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }) });
  const index = createRoute({ getParentRoute: () => protectedRoute, path: "/" });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([index])]), history: createMemoryHistory({ initialEntries: ["/"] }) });
  await router.load();
  const scope = { queryClient: client, sessionId: "session", userId: "user" };
  const stream = getContainerLogStream(selection, scope);
  render(<QueryClientProvider client={client}><DbProvider client={getDbClient(client)}><RouterContextProvider router={router}>
    <ContainerLogs selection={selection} />
  </RouterContextProvider></DbProvider></QueryClientProvider>);
  try {
    await act(async () => (sources.at(-1) ?? expect.fail("The viewer did not open its log stream")).dispatchEvent(new Event("live")));
    await waitFor(() => expect(screen.getByLabelText(/^Search/)).toBeTruthy());
    await run(getRuntimeCollections("acme", scope).services);
  } finally {
    cleanup();
    await stream.collection.cleanup();
    await getDbClient(client).cleanup();
    client.clear();
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  }
}

const record = (identity: string, containers: ReturnType<typeof container>[]) => ({
  id: identity, identity, serviceId: "ae4d79d2ba6b4c53b2297d5a40ef5c92", containers, hookContainers: [], observedAt: new Date().toISOString(),
});

it("shows the off states for the Service the selection names, and not for another Service's containers", async () => {
  const selection = { organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production", serviceId: "e0eccf29-1258-4c57-829f-2250eec054c4", serviceIdentity: SERVICE };
  await withViewer(selection, async (services) => {
    await act(async () => { services.insert(record("shop-production/worker", [container("elsewhere", "local")])); });
    expect(screen.queryByText("This service isn’t sending logs yet")).toBeNull();
    await act(async () => { services.insert(record(SERVICE, [container("old", "local"), container("quiet", "json-file")])); });
    await waitFor(() => expect(screen.getByText("This service isn’t sending logs yet")).toBeTruthy());
    await waitFor(() => expect(screen.getByText(/hasn’t sent logs for 10 minutes/)).toBeTruthy());
  });
});

it("searches only the loaded lines on a Deployment's card, since Cloud's lines name no Deployment", async () => {
  const selection = { organizationSlug: "acme", deploymentId: "00000000-0000-4000-8000-000000000001", serviceId: "00000000-0000-4000-8000-000000000002", serviceIdentity: SERVICE };
  await withViewer(selection, async () => {
    expect(screen.getByLabelText("Search loaded logs")).toBeTruthy();
    expect(screen.queryByText("Log history isn’t set up")).toBeNull();
  });
});
