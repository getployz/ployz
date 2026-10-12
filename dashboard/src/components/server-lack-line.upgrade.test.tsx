// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { DbProvider } from "@tanstack/react-db";
import { createMemoryHistory, createRootRoute, createRoute, createRouter, RouterContextProvider } from "@tanstack/react-router";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { getDbClient } from "#/collections/scope";
import { ServerLacks, requirements } from "#/modules/runtime/daemon-compat";
import { getRuntimeCollections, type RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { listLatestServerUpgradesServerFn, readChannelReleaseServerFn, requestServerUpgradeServerFn } from "#/modules/server-upgrade/server-upgrade.functions";
import { ServerLackLine } from "./server-lack-line";

vi.mock("#/modules/server-upgrade/server-upgrade.functions", () => ({
  listLatestServerUpgradesServerFn: vi.fn(),
  readChannelReleaseServerFn: vi.fn(),
  requestServerUpgradeServerFn: vi.fn(),
  setServerUpgradeSettingsServerFn: vi.fn(),
}));
vi.mock("#/modules/server-upgrade/server-upgrade.hooks", async (actual) => ({
  ...await actual<typeof import("#/modules/server-upgrade/server-upgrade.hooks")>(),
  useServerUpgradeSettings: () => ({ automatic: false, channel: "stable" }),
}));

afterEach(() => { cleanup(); vi.useRealTimers(); vi.resetAllMocks(); });

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
  return async () => { cleanup(); await getDbClient(client).cleanup(); client.clear(); };
}

it("names the channel's release and offers Upgrade once the release reads, and Upgrade asks Cloud for that Server", async () => {
  vi.mocked(listLatestServerUpgradesServerFn).mockResolvedValue({ servers: {} });
  vi.mocked(readChannelReleaseServerFn).mockResolvedValue("0.2.4");
  vi.mocked(requestServerUpgradeServerFn).mockResolvedValue(undefined as never);
  const teardown = await renderLine();
  try {
    expect((await screen.findByRole("button", { name: "Upgrade" })).closest("[role=status]")?.textContent)
      .toBe("Logs need a newer Ployz on web-2. 0.2.4 is out.Upgrade");
    expect(readChannelReleaseServerFn).toHaveBeenCalledWith({ data: { channel: "stable", line: "v0" } });
    fireEvent.click(screen.getByRole("button", { name: "Upgrade" }));
    expect(requestServerUpgradeServerFn).toHaveBeenCalledWith({ data: { organizationSlug: "acme", machineId: "m-2" } });
    expect(screen.getByRole("status").textContent).toBe("Upgrading web-2 to 0.2.4");
  } finally {
    await teardown();
  }
});

it("tries again after a read under the line fails, so the release and button come back without a reload", async () => {
  vi.mocked(listLatestServerUpgradesServerFn).mockRejectedValueOnce(new Error("fetch failed")).mockResolvedValue({ servers: {} });
  vi.mocked(readChannelReleaseServerFn).mockResolvedValue("0.2.4");
  vi.spyOn(console, "error").mockImplementation(() => {});
  vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
  const teardown = await renderLine();
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
