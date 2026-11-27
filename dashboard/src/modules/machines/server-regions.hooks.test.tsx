// @vitest-environment jsdom
import type { ConfigView } from "@ployz/sdk";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import type { StoreResult } from "#/modules/config-store/store.contract";
import * as functions from "#/modules/config-store/store.functions";
import { getDbClient } from "#/collections/scope";
import { applyRuntimeSnapshot, getRuntimeCollections, type RuntimeIncompleteIds, type RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { useNewEnvironmentRegion, useRegionOptions } from "./server-regions.hooks";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

const founder: RuntimeMachineRecord = {
  id: "1".repeat(32), name: "fsn-1", subnet: "10.210.0.0/24", joinedAt: null, publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: false,
  acceptsServices: true, storage: "pool", buildConcurrency: null, region: "default", effectiveBuildConcurrency: 1, runningBuilds: 0,
  daemonVersion: "0.4.0-beta.2", capabilities: [], poolCommittableBytes: null, sealing: null, membership: "up", unanswered: false,
  observedContainerCount: 0, observedAt: "2026-11-19T00:00:00Z",
};

async function newEnvironmentRegion(unread: Partial<RuntimeIncompleteIds>) {
  const queryClient = new QueryClient();
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  applyRuntimeSnapshot({
    collections: getRuntimeCollections("acme", scope),
    snapshot: {
      connection: { status: "observed", observedAt: founder.observedAt }, machines: [founder], services: [], volumeCopies: [],
      certificates: [], published: [], incompleteIds: { machines: [], containers: [], volumes: [], certificates: [], ...unread },
    },
  });
  const shown = renderHook(() => useNewEnvironmentRegion("acme"));
  try {
    await waitFor(() => expect(shown.result.current.settled).toBe(true));
    return shown.result.current.region;
  } finally {
    shown.unmount();
    await getDbClient(queryClient).cleanup();
  }
}

it("starts a new Environment in the first Server's region though a Container went unread", async () => {
  expect(await newEnvironmentRegion({ containers: ["c".repeat(64)] })).toBe("default");
});

it("gives a new Environment no region while a Machine went unread", async () => {
  expect(await newEnvironmentRegion({ machines: ["2".repeat(32)] })).toBeNull();
});

const server = (name: string, region: string): RuntimeMachineRecord => ({
  id: name.padEnd(32, "0"), name, subnet: null, joinedAt: null, publicIp: null, acceptsIngress: true, endpoints: [], acceptsBuilds: false,
  acceptsServices: true, storage: "pool", buildConcurrency: null, region, effectiveBuildConcurrency: 1, runningBuilds: 0,
  daemonVersion: "0.4.0-beta.2", capabilities: [], poolCommittableBytes: null, sealing: null, membership: "up", unanswered: false,
  observedContainerCount: 0, observedAt: "2026-11-19T00:00:00Z",
});

it("reads the region names once on a cold load, after the Servers arrive", async () => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  const regions: StoreResult<ConfigView> = { ok: true, value: { view: "regions", regions: [{ key: "fsn", name: "Falkenstein" }] } };
  const read = vi.spyOn(functions, "readStoreViewServerFn").mockResolvedValue(regions);
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  const shown = renderHook(() => useRegionOptions("acme"), { wrapper });
  try {
    await act(async () => {
      applyRuntimeSnapshot({
        collections: getRuntimeCollections("acme", scope),
        snapshot: {
          connection: { status: "observed", observedAt: "2026-11-19T00:00:00Z" }, machines: [server("fsn-1", "fsn"), server("syd-1", "syd")],
          services: [], volumeCopies: [], certificates: [], published: [],
          incompleteIds: { machines: [], containers: [], volumes: [], certificates: [] },
        },
      });
    });
    await waitFor(() => expect(shown.result.current.map(({ name }) => name)).toEqual(["Any region", "Falkenstein", "syd"]));
    expect(read.mock.calls.map(([call]) => call.data.query)).toEqual([{ query: "regions", labels: ["default", "fsn", "syd"] }]);
  } finally {
    shown.unmount();
    await getDbClient(queryClient).cleanup();
  }
});
