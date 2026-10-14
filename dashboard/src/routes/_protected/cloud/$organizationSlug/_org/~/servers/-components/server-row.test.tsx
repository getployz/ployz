// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { serverStatus } from "#/modules/machines/server-status";
import type { Server } from "#/modules/machines/use-servers";
import type { RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import type { ServerUpgradeLine } from "#/modules/server-upgrade/server-upgrade";
import { ServerRow, serverRowState } from "./server-row";

beforeEach(() => {
  // Narrow viewport: the meters never read, so these rows show only what the Runtime says.
  vi.stubGlobal("matchMedia", (query: string) => ({ matches: false, media: query, addEventListener: () => {}, removeEventListener: () => {} }));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

function server(input: { membership?: RuntimeMachineRecord["membership"]; services?: string[]; storage?: RuntimeMachineRecord["storage"]; acceptsBuilds?: boolean } = {}): Server {
  const machine: RuntimeMachineRecord = {
    id: "1".repeat(32), name: "fsn-1", publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: input.acceptsBuilds ?? false,
    acceptsServices: true, storage: input.storage ?? "pool", buildConcurrency: null, effectiveBuildConcurrency: 1, runningBuilds: 0,
    daemonVersion: "0.3.0-beta.2", capabilities: [], membership: input.membership ?? "up", observedContainerCount: 0, observedAt: "2026-10-14T00:00:00Z",
  };
  const services = (input.services ?? []).map((name) => ({
    identity: `shop/${name}`, name, namespace: "shop", machineIds: new Set([machine.id]), global: false, runningOn: new Set([machine.id]),
  }));
  return { machine, name: machine.name, status: serverStatus(machine), services };
}

async function row(shown: Server, upgrade: ServerUpgradeLine = null) {
  const root = createRootRoute({ component: () => <ServerRow organizationSlug="acme" server={shown} upgrade={upgrade} stale={false} /> });
  const router = createRouter({ routeTree: root, history: createMemoryHistory({ initialEntries: ["/"] }) });
  render(<QueryClientProvider client={new QueryClient()}><RouterProvider router={router} /></QueryClientProvider>);
  return screen.findByRole("link");
}

it("says nothing about a healthy Server beyond its name and what runs there", async () => {
  const link = await row(server({ services: ["api", "web"] }));
  expect(link.textContent).toBe("fsn-1apiweb");
  expect(link.getAttribute("href")).toBe(`/cloud/acme/~/servers/${"1".repeat(32)}`);
});

it("shows three Services and counts the rest", async () => {
  const link = await row(server({ services: ["api", "web", "worker", "postgres", "redis"] }));
  expect(link.textContent).toBe("fsn-1apiwebworker+2");
});

it("says what an idle Server does, and that a Docker-only Server takes no volumes", async () => {
  expect((await row(server({ acceptsBuilds: true }))).textContent).toBe("fsn-1Only runs builds");
  cleanup();
  expect((await row(server({ storage: "stateless" }))).textContent).toBe("fsn-1Nothing runningDocker only");
});

it("names the state that needs someone", async () => {
  expect((await row(server({ membership: "down", services: ["api"] }))).textContent).toBe("fsn-1apiOffline");
  cleanup();
  expect((await row(server({ services: ["api"] }), { kind: "upgrading", target: "0.3.0" })).textContent).toBe("fsn-1apiUpgrading");
  cleanup();
  expect((await row(server({ services: ["api"] }), { kind: "behind", release: "0.3.0", canUpgrade: true })).textContent).toBe("fsn-1api");
});

it("lets a running upgrade explain the Server, and offline outrank a failed one", () => {
  const failed: ServerUpgradeLine = { kind: "failed", target: "0.3.0", nothingChanged: true, details: null, canRetry: true };
  expect(serverRowState("offline", { kind: "upgrading", target: "0.3.0" })).toBe("upgrading");
  expect(serverRowState("offline", failed)).toBe("offline");
  expect(serverRowState("unknown", null)).toBe("unknown");
  expect(serverRowState("online", failed)).toBe("upgrade-failed");
  expect(serverRowState("building", null)).toBe("building");
  expect(serverRowState("online", { kind: "when-back", release: "0.3.0" })).toBe("online");
});
