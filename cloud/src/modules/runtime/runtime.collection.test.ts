import { describe, expect, it } from "vitest";
import {
  applyRuntimeSnapshot,
  CLUSTER_UNREACHABLE_ERROR,
  getCachedRuntimeSnapshot,
  projectRuntimeMachineRecord,
  unavailableRuntimeSnapshot,
  unreachableRuntimeSnapshot,
  type RuntimeSnapshotLens,
} from "#/modules/runtime/runtime.collection";

describe("projectRuntimeMachineRecord", () => {
  it("accepts cached records written before storage evidence was added", () => {
    expect(
      projectRuntimeMachineRecord({
        id: "m1",
        name: "node-1",
        publicIp: null,
        gateway: { status: "not_installed" },
        observedContainerCount: null,
        region: null,
        availabilityZone: null,
        overlayIp: null,
        endpoints: [],
        testimonyStatus: "no_answer",
        lastObservedAt: null,
        updatedAt: "2026-07-01T00:00:00.000Z",
      }),
    ).not.toHaveProperty("storage");
  });
});

describe("applyRuntimeSnapshot", () => {
  it("fans one full snapshot into every runtime collection cache", () => {
    const snapshot: RuntimeSnapshotLens = {
      status: "live_empty",
      error: null,
      publicUrl: {
        mode: "disabled",
        domain: null,
        leaseApex: null,
        dnsTarget: {
          intent: "disabled",
          allocation: "unacquired",
          publication: "unpublished",
        },
      },
      machines: [],
      services: [],
      updatedAt: "2026-07-01T00:00:00.000Z",
    };

    applyRuntimeSnapshot({
      organizationSlug: "acme",
      snapshot,
    });

    expect(
      getCachedRuntimeSnapshot({ organizationSlug: "acme" }),
    ).toEqual(snapshot);
  });
});

describe("unavailableRuntimeSnapshot", () => {
  it("retains previous machines/services and updatedAt", () => {
    const previous: RuntimeSnapshotLens = {
      status: "live_rows",
      error: null,
      publicUrl: {
        mode: "ployz",
        domain: "brisk-river.up.ployz.app",
        leaseApex: "brisk-river.up.ployz.app",
        dnsTarget: {
          intent: "enabled",
          allocation: "allocated",
          publication: "applied",
        },
      },
      machines: [
        {
          id: "m1",
          name: "node-1",
          publicIp: null,
          gateway: { status: "silent", reason: "no_answer" },
          observedContainerCount: null,
          region: null,
          availabilityZone: null,
          overlayIp: null,
          endpoints: [],
          testimonyStatus: "answered",
          lastObservedAt: "2026-07-01T00:00:00.000Z",
          updatedAt: "2026-07-01T00:00:00.000Z",
        },
      ],
      services: [
        {
          id: "s1",
          namespaceId: "ns1",
          serviceId: "svc1",
          activeRevisionId: "rev1",
          routeCount: 1,
          instanceCount: 1,
          readyInstanceCount: 1,
          bindings: [],
          updatedAt: "2026-07-01T00:00:00.000Z",
        },
      ],
      updatedAt: "2026-07-01T00:00:00.000Z",
    };

    const snapshot = unavailableRuntimeSnapshot(
      previous,
      "Runtime connection lost.",
    );

    expect(snapshot).toEqual({
      status: "unavailable",
      error: "Runtime connection lost.",
      publicUrl: previous.publicUrl,
      machines: previous.machines,
      services: previous.services,
      updatedAt: previous.updatedAt,
    });
  });
});

describe("unreachableRuntimeSnapshot", () => {
  it("clears machines so stale rows are not rendered as membership", () => {
    expect(unreachableRuntimeSnapshot(CLUSTER_UNREACHABLE_ERROR)).toMatchObject({
      status: "unreachable",
      error: CLUSTER_UNREACHABLE_ERROR,
      machines: [],
      services: [],
    });
  });
});
