// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { DbProvider } from "@tanstack/react-db";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterContextProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { watchOrganizationChanges } from "#/collections/org-changes.stream";
import { getDbClient } from "#/collections/scope";
import { ServerLacks, requirements } from "#/modules/runtime/daemon-compat";
import { getRuntimeCollections, type RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import * as functions from "#/modules/server-upgrade/server-upgrade.functions";
import * as hooks from "#/modules/server-upgrade/server-upgrade.hooks";
import { ServerLackLine } from "./server-lack-line";

afterEach(() => { cleanup(); vi.useRealTimers(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

function stubCloud() {
  vi.spyOn(hooks, "useServerUpgradeSettings").mockReturnValue({ automatic: false, channel: "stable", setAutomatic: () => {}, setChannel: () => {} });
  return {
    listLatest: vi.spyOn(functions, "listLatestServerUpgradesServerFn"),
    readRelease: vi.spyOn(functions, "readChannelReleaseServerFn").mockResolvedValue("0.2.4"),
    request: vi.spyOn(functions, "requestServerUpgradeServerFn").mockResolvedValue(undefined),
  };
}

const machine: RuntimeMachineRecord = {
  id: "m-2", name: "web-2", publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: true, acceptsServices: true, storage: "pool",
  buildConcurrency: null, effectiveBuildConcurrency: 1, runningBuilds: 0, daemonVersion: "0.2.3", capabilities: [], membership: "up",
  observedContainerCount: 0, observedAt: "2026-10-12T00:00:00Z",
};

async function renderLine() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const root = createRootRoute();
  const protectedRoute = createRoute({ getParentRoute: () => root, id: "_protected", beforeLoad: () => ({ session: { session: { id: "session" }, user: { id: "user" } } }) });
  const index = createRoute({ getParentRoute: () => protectedRoute, path: "/" });
  const router = createRouter({ routeTree: root.addChildren([protectedRoute.addChildren([index])]), history: createMemoryHistory({ initialEntries: ["/"] }) });
  await router.load();
  getRuntimeCollections("acme", { queryClient: client, sessionId: "session", userId: "user" }).machines.insert([machine]);
  render(<QueryClientProvider client={client}><DbProvider client={getDbClient(client)}><RouterContextProvider router={router}>
    <ServerLackLine organizationSlug="acme" name="m-2" lack={new ServerLacks({ machineId: "m-2", capability: requirements.liveLogs.capability })} />
  </RouterContextProvider></DbProvider></QueryClientProvider>);
  return { client, teardown: async () => { cleanup(); await getDbClient(client).cleanup(); client.clear(); } };
}

it("names the channel's release and offers Upgrade once the release reads, and Upgrade asks Cloud for that Server", async () => {
  const cloud = stubCloud();
  cloud.listLatest.mockResolvedValue({ servers: {} });
  const { teardown } = await renderLine();
  try {
    expect((await screen.findByRole("button", { name: "Upgrade" })).closest("[role=status]")?.textContent)
      .toBe("Logs need a newer Ployz on web-2. 0.2.4 is out.Upgrade");
    expect(cloud.readRelease).toHaveBeenCalledWith({ data: { channel: "stable", line: "v0" } });
    fireEvent.click(screen.getByRole("button", { name: "Upgrade" }));
    expect(cloud.request).toHaveBeenCalledWith({ data: { organizationSlug: "acme", machineId: "m-2" } });
    expect(screen.getByRole("status").textContent).toBe("Upgrading web-2 to 0.2.4");
  } finally {
    await teardown();
  }
});

it("waits for another Server's running upgrade instead of offering Upgrade", async () => {
  stubCloud().listLatest.mockResolvedValue({ servers: { "m-1": {
    attemptId: "b".repeat(32), outcome: "running", stage: null, error: null, fromVersion: "0.2.3", targetVersion: "0.2.4", startedAt: new Date().toISOString(),
  } } });
  const { teardown } = await renderLine();
  try {
    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("Logs need a newer Ployz on web-2. 0.2.4 is out.Waits for the running upgrade"));
    expect(screen.queryByRole("button", { name: "Upgrade" })).toBeNull();
  } finally {
    await teardown();
  }
});

it("tries again after a read under the line fails, so the release and button come back without a reload", async () => {
  stubCloud().listLatest.mockRejectedValueOnce(new Error("fetch failed")).mockResolvedValue({ servers: {} });
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const { teardown } = await renderLine();
  try {
    await act(async () => { await vi.advanceTimersByTimeAsync(100); });
    expect(screen.getByRole("status").textContent).toBe("Logs need a newer Ployz on web-2.");
    expect(screen.queryByRole("button")).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(10_000); });
    await act(async () => { await vi.advanceTimersByTimeAsync(100); });
    expect(screen.getByRole("status").textContent).toBe("Logs need a newer Ployz on web-2. 0.2.4 is out.Upgrade");
  } finally {
    await teardown();
  }
});

it("moves to Upgrading when another session's Upgrade lands through the org change stream, with no click here", async () => {
  const sources: EventTarget[] = [];
  vi.stubGlobal("EventSource", class extends EventTarget { constructor() { super(); sources.push(this); } close() {} });
  const cloud = stubCloud();
  cloud.listLatest.mockResolvedValue({ servers: {} });
  const { client, teardown } = await renderLine();
  const stop = watchOrganizationChanges("acme", { queryClient: client, sessionId: "session", userId: "user" });
  try {
    await screen.findByRole("button", { name: "Upgrade" });
    cloud.listLatest.mockResolvedValue({ servers: { "m-2": {
      attemptId: "a".repeat(32), outcome: "running", stage: null, error: null, fromVersion: "0.2.3", targetVersion: "0.2.4", startedAt: new Date().toISOString(),
    } } });
    await act(async () => { sources.at(-1)?.dispatchEvent(new MessageEvent("changes", { data: JSON.stringify({ collections: ["server_upgrade"] }) })); });
    await waitFor(() => expect(screen.getByRole("status").textContent).toBe("Upgrading web-2 to 0.2.4"));
    expect(cloud.request).not.toHaveBeenCalled();
  } finally {
    stop();
    await teardown();
  }
});
