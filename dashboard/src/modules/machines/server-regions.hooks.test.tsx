// @vitest-environment jsdom
import { QueryClient } from "@tanstack/react-query";
import { cleanup, renderHook, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { getDbClient } from "#/collections/scope";
import { applyRuntimeSnapshot, getRuntimeCollections, type RuntimeIncompleteIds, type RuntimeMachineRecord } from "#/modules/runtime/runtime.collection";
import { useNewEnvironmentRegion } from "./server-regions.hooks";

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
      status: "observed", error: null, machines: [founder], services: [], volumeCopies: [], certificates: [], published: [],
      incompleteIds: { machines: [], containers: [], volumes: [], certificates: [], ...unread }, observedAt: founder.observedAt,
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
