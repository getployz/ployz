import { QueryClient } from "@tanstack/react-query";
import { getDbClient } from "#/collections/scope";
import { getRuntimeCollections } from "./runtime.collection";
import { describe, expect, it } from "vitest";
import {
  applyRuntimeSnapshot,
  getCachedRuntimeSnapshot,
  projectRuntimeMachineRecord,
  unavailableRuntimeSnapshot,
  unreachableRuntimeSnapshot,
  type RuntimeSnapshot,
} from "#/modules/runtime/runtime.collection";

function observedSnapshot(): RuntimeSnapshot {
  return {
    connection: { status: "observed", observedAt: "2026-07-01T00:00:00.000Z" },
    machines: [
      {
        id: "m1",
        name: "node-1",
        subnet: null, joinedAt: null, publicIp: null,
        acceptsIngress: true,
        endpoints: [],
        acceptsBuilds: true, acceptsServices: true, storage: null,
        buildConcurrency: null, region: null,
        effectiveBuildConcurrency: 1,
        runningBuilds: 0,
        daemonVersion: "0.2.1",
        capabilities: [],
        poolCommittableBytes: null,
        sealing: null,
        membership: "up",
        unanswered: false,
        observedContainerCount: 1,
        observedAt: "2026-07-01T00:00:00.000Z",
      },
    ],
    services: [
      {
        id: "production/api",
        identity: "production/api",
        serviceId: "runtime-api",
        containers: [
          {
            id: "ctr-1",
            displayName: "api-1",
            machineId: "m1",
            namespace: "production",
            kind: "service_container",
          },
        ],
        hookContainers: [],
        observedAt: "2026-07-01T00:00:00.000Z",
      },
    ],
    volumeCopies: [{ machineId: "m1", name: "app_vol-1", role: "slot", slotRole: "mirror", boundBytes: 10_000_000_000 }],
    certificates: [
      {
        hostname: "api.example.test",
        status: "pending",
        lastError: null,
        backoff: null,
        viaProxy: false,
      },
    ],
    published: ["api.example.test"],
    incompleteIds: {
      machines: [],
      containers: [],
      volumes: [],
      certificates: [],
    },
  };
}

describe("projectRuntimeMachineRecord", () => {
  it("returns only direct Machine evidence", () => {
    expect(
      projectRuntimeMachineRecord({
        id: "m1",
        name: "node-1",
        subnet: null, joinedAt: null, publicIp: null,
        acceptsIngress: true,
        endpoints: [],
        acceptsBuilds: true, acceptsServices: true, storage: null,
        buildConcurrency: null, region: null,
        effectiveBuildConcurrency: 1,
        runningBuilds: 0,
        daemonVersion: "0.2.1",
        capabilities: [],
        poolCommittableBytes: null,
        sealing: null,
        membership: "down",
        unanswered: false,
        observedContainerCount: 0,
        observedAt: "2026-07-01T00:00:00.000Z",
      }),
    ).toEqual({
      id: "m1",
      name: "node-1",
      subnet: null, joinedAt: null, publicIp: null,
      acceptsIngress: true,
      endpoints: [],
      acceptsBuilds: true, acceptsServices: true, storage: null,
      buildConcurrency: null, region: null,
      effectiveBuildConcurrency: 1,
      runningBuilds: 0,
      daemonVersion: "0.2.1",
      capabilities: [],
      poolCommittableBytes: null,
      sealing: null,
      membership: "down",
      unanswered: false,
      observedContainerCount: 0,
      observedAt: "2026-07-01T00:00:00.000Z",
    });
  });
});

describe("applyRuntimeSnapshot", () => {
  it("fans one direct watch observation into the local collections", () => {
    const snapshot = observedSnapshot();
    const collections = getRuntimeCollections("runtime-observation", { queryClient: new QueryClient(), sessionId: "session", userId: "user" });
    applyRuntimeSnapshot({ collections, snapshot });

    expect(
      getCachedRuntimeSnapshot(collections),
    ).toEqual(snapshot);
  });

  it("moves between connection states that carry different fields", () => {
    const collections = getRuntimeCollections("runtime-transitions", { queryClient: new QueryClient(), sessionId: "session", userId: "user" });
    const steps = [
      unreachableRuntimeSnapshot("cluster_unreachable"),
      observedSnapshot(),
      unavailableRuntimeSnapshot(observedSnapshot()),
      unreachableRuntimeSnapshot("awaits_key"),
    ];
    for (const snapshot of steps) {
      applyRuntimeSnapshot({ collections, snapshot });
      expect(getCachedRuntimeSnapshot(collections)).toEqual(snapshot);
    }
  });

  it("isolates observations between requests and sessions in the same organization", async () => {
    const firstQuery = new QueryClient();
    const secondQuery = new QueryClient();
    const scope = { queryClient: firstQuery, sessionId: "first", userId: "user" };
    const first = getRuntimeCollections("organization", scope);
    const second = getRuntimeCollections("organization", { ...scope, queryClient: secondQuery });
    const nextSession = getRuntimeCollections("organization", { ...scope, sessionId: "second" });
    applyRuntimeSnapshot({ collections: first, snapshot: observedSnapshot() });
    expect(getCachedRuntimeSnapshot(first)?.connection.status).toBe("observed");
    expect(getCachedRuntimeSnapshot(second)?.connection.status).toBe("connecting");
    expect(getCachedRuntimeSnapshot(nextSession)?.connection.status).toBe("connecting");
    await Promise.all([getDbClient(firstQuery).cleanup(), getDbClient(secondQuery).cleanup()]);
    firstQuery.clear(); secondQuery.clear();
  });
});

describe("unavailableRuntimeSnapshot", () => {
  it("retains the last observation and marks it unavailable", () => {
    const previous = observedSnapshot();

    expect(
      unavailableRuntimeSnapshot(previous),
    ).toEqual({
      ...previous,
      connection: { status: "unavailable", observedAt: "2026-07-01T00:00:00.000Z" },
    });
  });

  it("has no observation time before any observation", () => {
    expect(unavailableRuntimeSnapshot(null).connection).toEqual({ status: "unavailable", observedAt: null });
  });
});

describe("unreachableRuntimeSnapshot", () => {
  it("clears observation rows so they are not rendered as membership", () => {
    expect(unreachableRuntimeSnapshot("cluster_unreachable")).toEqual({
      connection: { status: "unreachable", reason: "cluster_unreachable" },
      machines: [],
      services: [],
      volumeCopies: [],
      certificates: [],
      published: [],
      incompleteIds: {
        machines: [],
        containers: [],
        volumes: [],
        certificates: [],
      },
    });
  });
});
