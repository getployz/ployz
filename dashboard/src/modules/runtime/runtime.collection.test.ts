import { describe, expect, it } from "vitest";
import {
  applyRuntimeSnapshot,
  CLUSTER_UNREACHABLE_ERROR,
  getCachedRuntimeSnapshot,
  projectRuntimeMachineRecord,
  unavailableRuntimeSnapshot,
  unreachableRuntimeSnapshot,
  type RuntimeSnapshot,
} from "#/modules/runtime/runtime.collection";

function observedSnapshot(): RuntimeSnapshot {
  return {
    status: "observed",
    error: null,
    hostedDnsHostname: "brisk-river.up.ployz.app",
    machines: [
      {
        id: "m1",
        name: "node-1",
        publicIp: null,
        endpoints: [],
        membership: "up",
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
            projectName: "production",
            kind: "service_container",
          },
        ],
        hookContainers: [],
        observedAt: "2026-07-01T00:00:00.000Z",
      },
    ],
    certificates: [
      {
        hostname: "api.example.test",
        status: "pending",
        lastError: null,
        backoff: null,
      },
    ],
    incompleteIds: {
      machines: [],
      containers: [],
      volumes: [],
      certificates: [],
    },
    observedAt: "2026-07-01T00:00:00.000Z",
  };
}

describe("projectRuntimeMachineRecord", () => {
  it("returns only direct Machine evidence", () => {
    expect(
      projectRuntimeMachineRecord({
        id: "m1",
        name: "node-1",
        publicIp: null,
        endpoints: [],
        membership: "down",
        observedContainerCount: 0,
        observedAt: "2026-07-01T00:00:00.000Z",
      }),
    ).toEqual({
      id: "m1",
      name: "node-1",
      publicIp: null,
      endpoints: [],
      membership: "down",
      observedContainerCount: 0,
      observedAt: "2026-07-01T00:00:00.000Z",
    });
  });
});

describe("applyRuntimeSnapshot", () => {
  it("fans one direct watch observation into the local collections", () => {
    const snapshot = observedSnapshot();
    applyRuntimeSnapshot({ organizationSlug: "runtime-observation", snapshot });

    expect(
      getCachedRuntimeSnapshot({ organizationSlug: "runtime-observation" }),
    ).toEqual(snapshot);
  });
});

describe("unavailableRuntimeSnapshot", () => {
  it("retains the last observation and marks it unavailable", () => {
    const previous = observedSnapshot();

    expect(
      unavailableRuntimeSnapshot(previous, "Runtime connection lost."),
    ).toEqual({
      ...previous,
      status: "unavailable",
      error: "Runtime connection lost.",
    });
  });
});

describe("unreachableRuntimeSnapshot", () => {
  it("clears observation rows so they are not rendered as membership", () => {
    expect(unreachableRuntimeSnapshot(CLUSTER_UNREACHABLE_ERROR)).toEqual({
      status: "unreachable",
      error: CLUSTER_UNREACHABLE_ERROR,
      hostedDnsHostname: null,
      machines: [],
      services: [],
      certificates: [],
      incompleteIds: {
        machines: [],
        containers: [],
        volumes: [],
        certificates: [],
      },
      observedAt: null,
    });
  });
});
