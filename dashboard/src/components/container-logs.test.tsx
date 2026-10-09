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

const bodyOf = (init: RequestInit | undefined) => JSON.parse(String(init?.body)) as { cursor?: string; before?: string };
const cursorOf = (init: RequestInit | undefined) => bodyOf(init).cursor;
const line = { kind: "line", id: "m/c/100/0", timestamp: "100", machineId: "m", machineName: "Server", containerId: "c", serviceName: "api", channel: "stdout", level: "info", message: "hello" } as const;

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
    if (cursorOf(init) !== undefined) return Response.json({ rows: [], failures: [], cursor: null });
    return Response.json({ rows: bodyOf(init).before === undefined ? [] : [{ ...line, id: "store/m/c/50/0", timestamp: "50" }], failures: [], cursor: "older" });
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
    // The tail arrives before the stream turns live.
    await act(async () => source.dispatchEvent(new MessageEvent("log", { data: JSON.stringify({ type: "record", record: line }) })));
    await act(async () => source.dispatchEvent(new Event("live")));
    // Streamed lines land in a batch a moment later.
    await waitFor(() => expect(stream.collection.size).toBe(2));
    expect(screen.queryByRole("button", { name: "Load older" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Refresh" })).toBeNull();
    // A short tail reads the Log Store's newest page at once, then before the tail that page sat inside.
    await waitFor(() => expect(fetchHistory).toHaveBeenCalledTimes(2));
    expect(bodyOf(fetchHistory.mock.calls[0]?.[1])).not.toHaveProperty("cursor");
    expect(bodyOf(fetchHistory.mock.calls[0]?.[1])).not.toHaveProperty("before");
    expect(bodyOf(fetchHistory.mock.calls[1]?.[1]).before).toBe("100");
    fireEvent.change(screen.getByLabelText("Search loaded logs"), { target: { value: "missing" } });
    expect(screen.getByText("No logs match your filters")).toBeTruthy();
    expect(screen.getByText("Scroll up or press Home to check older logs.")).toBeTruthy();
    fireEvent.wheel(screen.getByLabelText("Container logs"), { deltaY: -100 });
    await waitFor(() => expect(fetchHistory).toHaveBeenCalledTimes(3));
    expect(cursorOf(fetchHistory.mock.calls[2]?.[1])).toBe("older");
    await waitFor(() => expect(stream.getSnapshot().historyPending).toBe(false));
    expect(screen.queryByText("Scroll up or press Home to check older logs.")).toBeNull();
    expect(stream.collection.size).toBe(2);
    const opened = sources.length;
    firstView.unmount();
    await waitFor(() => expect(stream.collection.subscriberCount).toBe(0));
    expect(source.closed).toBe(false);
    expect(stream.collection.size).toBe(2);
    mount();
    await act(async () => { await stream.loadOlder(); });
    expect(fetchHistory).toHaveBeenCalledTimes(3);
    expect(sources).toHaveLength(opened);
    // Only an offline organization is said; the stream stays open and the lines stay.
    await act(async () => source.dispatchEvent(new Event("offline")));
    expect(screen.getByText(/Your servers are offline/)).toBeTruthy();
    await act(async () => source.dispatchEvent(new Event("live")));
    expect(screen.queryByText(/Your servers are offline/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Reconnect" })).toBeNull();
    expect(stream.collection.size).toBe(2);
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

it("says a refused log stream failed instead of loading forever, and keeps retrying", async () => {
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const sources: FakeEventSource[] = [];
  class FakeEventSource extends EventTarget {
    static CLOSED = 2;
    readyState = 0;
    constructor() { super(); sources.push(this); }
    close() { this.readyState = 2; }
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  vi.stubGlobal("fetch", async () => Response.json({ rows: [], failures: [], cursor: null }));
  const client = new QueryClient();
  const root = createRootRoute({ loader: () => ({ timeZone: "UTC" }) });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }) });
  const index = createRoute({ getParentRoute: () => protectedRoute, path: "/" });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([index])]), history: createMemoryHistory({ initialEntries: ["/"] }) });
  await router.load();
  const selection = { organizationSlug: "acme", environmentSlug: "refused" };
  const stream = getContainerLogStream(selection, { queryClient: client, sessionId: "session", userId: "user" });
  try {
    render(<QueryClientProvider client={client}><DbProvider client={getDbClient(client)}><RouterContextProvider router={router}>
      <ContainerLogs selection={selection} />
    </RouterContextProvider></DbProvider></QueryClientProvider>);
    const source = sources.at(-1);
    if (!source) throw new Error("Viewer did not open its log stream");
    expect(screen.getByLabelText("Loading logs")).toBeTruthy();
    await act(async () => { source.dispatchEvent(new Event("error")); });
    expect(screen.getByText("Couldn’t load logs")).toBeTruthy();
    await act(async () => source.dispatchEvent(new Event("live")));
    await act(async () => { await vi.advanceTimersByTimeAsync(10); });
    expect(screen.getByText("No logs yet")).toBeTruthy();
    await act(async () => { source.dispatchEvent(new Event("offline")); source.dispatchEvent(new Event("error")); });
    expect(screen.getByText("Your servers are offline")).toBeTruthy();
    expect(screen.queryByText("Couldn’t load logs")).toBeNull();
    await act(async () => source.dispatchEvent(new Event("live")));
    await act(async () => { source.readyState = 2; source.dispatchEvent(new Event("error")); });
    expect(screen.getByText("Couldn’t load logs")).toBeTruthy();
    await act(async () => { vi.advanceTimersByTime(1_000); });
    const retried = sources.at(-1);
    expect(retried).not.toBe(source);
    await act(async () => retried?.dispatchEvent(new Event("live")));
    expect(screen.getByText("No logs yet")).toBeTruthy();
  } finally {
    cleanup();
    await stream.collection.cleanup();
    await getDbClient(client).cleanup(); client.clear(); vi.unstubAllGlobals(); vi.useRealTimers();
  }
});

it("offers a service or server filter only once there is more than one to pick", async () => {
  const sources: FakeEventSource[] = [];
  class FakeEventSource extends EventTarget {
    close() {}
    constructor() { super(); sources.push(this); }
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const client = new QueryClient();
  const root = createRootRoute({ loader: () => ({ timeZone: "UTC" }) });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }) });
  const index = createRoute({ getParentRoute: () => protectedRoute, path: "/" });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([index])]), history: createMemoryHistory({ initialEntries: ["/"] }) });
  await router.load();
  const selection = { organizationSlug: "acme", environmentSlug: "varying" };
  const stream = getContainerLogStream(selection, { queryClient: client, sessionId: "session", userId: "user" });
  const line = (id: string, serviceName: string, machineName: string, message: string) => new MessageEvent("log", { data: JSON.stringify({ type: "record", record: {
    kind: "line", id, timestamp: id, machineId: machineName, machineName, containerId: serviceName, serviceName, channel: "stdout", level: "info", message,
  } }) });
  try {
    render(<QueryClientProvider client={client}><DbProvider client={getDbClient(client)}><RouterContextProvider router={router}>
      <ContainerLogs selection={selection} />
    </RouterContextProvider></DbProvider></QueryClientProvider>);
    const source = sources.at(-1);
    if (!source) throw new Error("Viewer did not open its log stream");
    // Lines naming their service and server follow the same rule; they render in a virtual list jsdom gives no height.
    await act(async () => source.dispatchEvent(line("100", "api", "hel-1", "listening")));
    await waitFor(() => expect(stream.collection.size).toBe(1));
    expect(screen.queryByLabelText("All services")).toBeNull();
    expect(screen.queryByLabelText("All servers")).toBeNull();
    await act(async () => source.dispatchEvent(line("200", "worker", "fsn-1", "ready")));
    await waitFor(() => expect(stream.collection.size).toBe(2));
    expect(screen.getByLabelText("All services")).toBeTruthy();
    expect(screen.getByLabelText("All servers")).toBeTruthy();
  } finally {
    cleanup();
    await stream.collection.cleanup();
    await getDbClient(client).cleanup(); client.clear(); vi.unstubAllGlobals();
  }
});

it("shows an old Deployment's stored lines with their gaps and missing servers, pages on scroll up, and filters by level", async () => {
  // jsdom lays nothing out; give the log a height so its virtual rows render.
  vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({ x: 0, y: 0, top: 0, left: 0, right: 800, bottom: 2_000, width: 800, height: 2_000, toJSON: () => ({}) });
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(2_000);
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(800);
  const sources: FakeEventSource[] = [];
  class FakeEventSource extends EventTarget {
    close() {}
    constructor() { super(); sources.push(this); }
  }
  vi.stubGlobal("EventSource", FakeEventSource);
  const line = (timestamp: string, level: string, message: string) => ({
    kind: "line", id: `store/hel-1/c/${timestamp}/0`, timestamp, machineId: "hel-1", machineName: "hel-1", containerId: "c", serviceName: "api", channel: "stderr", level, message,
  });
  const fetchHistory = vi.fn(async (_url: string, init: RequestInit) => Response.json(cursorOf(init) !== undefined ? { rows: [line("10", "error", "boom")], failures: [], cursor: null } : {
    rows: [line("50", "info", "listening"), line("60", "warn", "slow query"), { kind: "gap", id: "hel-1/c/gap/40", timestamp: "40", until: "45", machineId: "hel-1", machineName: "hel-1", containerId: "c", serviceName: "api", reason: "not_captured" }],
    failures: [{ machineId: "fsn-1", machineName: "fsn-1", message: "Its log service isn’t running." }],
    cursor: "page-2",
  }));
  vi.stubGlobal("fetch", fetchHistory);
  const client = new QueryClient();
  const root = createRootRoute({ loader: () => ({ timeZone: "UTC" }) });
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }) });
  const index = createRoute({ getParentRoute: () => protectedRoute, path: "/" });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([index])]), history: createMemoryHistory({ initialEntries: ["/"] }) });
  await router.load();
  const selection = { organizationSlug: "acme", deploymentId: "00000000-0000-4000-8000-000000000001" };
  const stream = getContainerLogStream(selection, { queryClient: client, sessionId: "session", userId: "user" });
  try {
    render(<QueryClientProvider client={client}><DbProvider client={getDbClient(client)}><RouterContextProvider router={router}>
      <ContainerLogs selection={selection} />
    </RouterContextProvider></DbProvider></QueryClientProvider>);
    const source = sources.at(-1);
    if (!source) throw new Error("Viewer did not open its log stream");
    // Its containers are gone, so the live tail is empty and the Log Store answers.
    await act(async () => source.dispatchEvent(new Event("live")));
    await waitFor(() => expect(screen.getByText("slow query")).toBeTruthy());
    expect(screen.getByText("Logs from this stretch weren’t captured.")).toBeTruthy();
    expect(screen.getByRole("alert").textContent).toContain("fsn-1: Its log service isn’t running.");
    expect(screen.queryByText("boom")).toBeNull();
    fireEvent.wheel(screen.getByLabelText("Container logs"), { deltaY: -100 });
    await waitFor(() => expect(screen.getByText("boom")).toBeTruthy());
    expect(bodyOf(fetchHistory.mock.calls[0]?.[1]).before).toBeUndefined();
    expect(cursorOf(fetchHistory.mock.calls[1]?.[1])).toBe("page-2");
    expect(screen.queryByRole("alert")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "error" }));
    expect(screen.getByText("boom")).toBeTruthy();
    expect(screen.queryByText("listening")).toBeNull();
    expect(screen.queryByText("slow query")).toBeNull();
    expect(screen.queryByText("Logs from this stretch weren’t captured.")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "warn" }));
    expect(screen.getByText("slow query")).toBeTruthy();
  } finally {
    cleanup();
    await stream.collection.cleanup();
    await getDbClient(client).cleanup(); client.clear(); vi.unstubAllGlobals(); vi.restoreAllMocks();
  }
});
