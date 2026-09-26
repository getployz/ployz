// @vitest-environment jsdom
import { StrictMode } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { DbProvider } from "@tanstack/react-db";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterContextProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { expect, it, vi } from "vitest";
import { getDbClient } from "#/collections/scope";
import { getContainerLogStream } from "#/modules/runtime/container-log.stream";
import { ContainerLogs } from "./container-logs";

it("retains logs and exhausted history across navigation, and reconnects only on failure", async () => {
  const sources: FakeEventSource[] = [];
  class FakeEventSource extends EventTarget {
    closed = false;
    constructor() { super(); sources.push(this); }
    close() { this.closed = true; }
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const fetchHistory = vi.fn(async (_url: string, init: RequestInit) => {
    expect(init.signal?.aborted).toBe(false);
    return Response.json({ records: [], errors: [] });
  });
  vi.stubGlobal("fetch", fetchHistory);
  const client = new QueryClient();
  const root = createRootRoute({ loader: () => ({ timeZone: "UTC" }) });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }) });
  const index = createRoute({ getParentRoute: () => protectedRoute, path: "/" });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([index])]), history: createMemoryHistory({ initialEntries: ["/"] }) });
  await router.load();
  const selection = { organizationSlug: "acme", environmentSlug: "production" };
  const stream = getContainerLogStream(selection, { queryClient: client, sessionId: "session", userId: "user" });
  const mount = () => render(<StrictMode><QueryClientProvider client={client}><DbProvider client={getDbClient(client)}><RouterContextProvider router={router}>
      <ContainerLogs selection={{ organizationSlug: "acme", environmentSlug: "production" }} />
    </RouterContextProvider></DbProvider></QueryClientProvider></StrictMode>);
  try {
    const firstView = mount();
    const source = sources.at(-1);
    if (!source) throw new Error("Viewer did not open its log stream");
    await act(async () => source.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
      id: "m/c/100/0", timestamp: "100", machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel: "stdout", message: "hello",
    } }) })));
    expect(screen.queryByRole("button", { name: "Load older" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Refresh" })).toBeNull();
    expect(fetchHistory).not.toHaveBeenCalled();
    fireEvent.wheel(screen.getByLabelText("Container logs"), { deltaY: -100 });
    await waitFor(() => expect(fetchHistory).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(stream.getSnapshot().historyPending).toBe(false));
    expect(stream.collection.size).toBe(1);
    const opened = sources.length;
    firstView.unmount();
    await waitFor(() => expect(stream.collection.subscriberCount).toBe(0));
    expect(source.closed).toBe(false);
    expect(stream.collection.size).toBe(1);
    mount();
    await act(async () => { await stream.loadOlder(); });
    expect(fetchHistory).toHaveBeenCalledTimes(1);
    expect(sources).toHaveLength(opened);
    // Only an offline organization is said; the stream stays open and the lines stay.
    await act(async () => source.dispatchEvent(new Event("offline")));
    expect(screen.getByText(/Your servers are offline/)).toBeTruthy();
    await act(async () => source.dispatchEvent(new Event("live")));
    expect(screen.queryByText(/Your servers are offline/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Reconnect" })).toBeNull();
    expect(stream.collection.size).toBe(1);
    expect(sources.filter(source => !source.closed)).toHaveLength(1);
    expect(getContainerLogStream(selection, { queryClient: client, sessionId: "session", userId: "user" })).toBe(stream);
  } finally {
    cleanup();
    await waitFor(() => expect(stream.collection.subscriberCount).toBe(0));
    await stream.collection.cleanup();
    expect(sources.every(source => source.closed)).toBe(true);
    expect(stream.signal.aborted).toBe(true);
    expect(stream.collection.size).toBe(0);
    await getDbClient(client).cleanup(); client.clear(); vi.unstubAllGlobals();
  }
});
