// @vitest-environment jsdom
import type { ConfigView } from "@ployz/sdk";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, renderHook, waitFor } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { getDbClient } from "#/collections/scope";
import type { StoreResult } from "#/modules/config-store/store.contract";
import * as functions from "#/modules/config-store/store.functions";
import { applyRuntimeSnapshot, getRuntimeCollections, type RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { useRegionOptions } from "./use-region-options";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

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
          status: "observed", error: null, machines: [server("fsn-1", "fsn"), server("syd-1", "syd")], services: [], volumeCopies: [],
          certificates: [], published: [], incompleteIds: { machines: [], containers: [], volumes: [], certificates: [] }, observedAt: null,
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
