import { describe, expect, it } from "vitest";
import type { RuntimeWatchView } from "@ployz/sdk";
import { Result } from "effect";
import { RUNTIME_PUBLIC_URL_NONE } from "#/modules/runtime/runtime.collection";
import { runtimeSnapshotLensFromWatchFrame } from "#/modules/runtime/runtime-watch-frame";
import {
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";

const OBSERVED_AT = "2026-08-18T00:00:00.000Z";

function project(input: RuntimeWatchView) {
  const projected = runtimeSnapshotLensFromWatchFrame(input);
  if (Result.isFailure(projected)) throw projected.failure;
  return projected.success;
}

describe("runtimeSnapshotLensFromWatchFrame", () => {
  it("projects an empty watch frame as live-empty with no stored services", () => {
    expect(project(runtimeWatchFrameFixture({ observed_at: OBSERVED_AT }))).toEqual({
      status: "live_empty",
      error: null,
      publicUrl: RUNTIME_PUBLIC_URL_NONE,
      machines: [],
      services: [],
      updatedAt: OBSERVED_AT,
    });
  });

  it("projects membership, endpoints, and per-machine container counts", () => {
    const lens = project(
      runtimeWatchFrameFixture({
        observed_at: OBSERVED_AT,
        machines: [
          runtimeWatchMachineObservationFixture({
            machine: runtimeWatchMachineFixture("machine-a", "edge-a", {
              public_ip: "203.0.113.10",
            }),
            membership: "up",
          }),
          runtimeWatchMachineObservationFixture({
            machine: runtimeWatchMachineFixture("machine-b", "edge-b"),
            membership: "down",
          }),
        ],
        containers: [
          runtimeWatchContainerFixture("machine-a", "ctr-1"),
          runtimeWatchContainerFixture("machine-a", "ctr-2"),
          runtimeWatchContainerFixture("machine-b", "ctr-3"),
        ],
      }),
    );

    expect(lens.status).toBe("live_rows");
    expect(lens.services).toEqual([]);
    expect(lens.machines).toEqual([
      {
        id: "machine-a",
        name: "edge-a",
        publicIp: "203.0.113.10",
        gateway: { status: "not_installed" },
        observedContainerCount: 2,
        region: null,
        availabilityZone: null,
        overlayIp: null,
        endpoints: ["udp://203.0.113.10:51820"],
        testimonyStatus: "answered",
        lastObservedAt: OBSERVED_AT,
        updatedAt: OBSERVED_AT,
      },
      {
        id: "machine-b",
        name: "edge-b",
        publicIp: null,
        gateway: { status: "not_installed" },
        observedContainerCount: 1,
        region: null,
        availabilityZone: null,
        overlayIp: null,
        endpoints: ["udp://203.0.113.10:51820"],
        testimonyStatus: "no_answer",
        lastObservedAt: null,
        updatedAt: OBSERVED_AT,
      },
    ]);
  });

  it("treats hosted DNS as an allocated Ployz public URL", () => {
    expect(
      project(
        runtimeWatchFrameFixture({
          observed_at: OBSERVED_AT,
          hosted_dns_hostname: "brisk-river.up.ployz.app",
        }),
      ).publicUrl,
    ).toEqual({
      mode: "ployz",
      domain: "brisk-river.up.ployz.app",
      leaseApex: "brisk-river.up.ployz.app",
      dnsTarget: {
        intent: "enabled",
        allocation: "allocated",
        publication: "applied",
      },
    });
  });

  it("rejects a frame that cannot project into the live lens", () => {
    const projected = runtimeSnapshotLensFromWatchFrame(
      runtimeWatchFrameFixture({
        observed_at: OBSERVED_AT,
        machines: [
          runtimeWatchMachineObservationFixture({
            machine: runtimeWatchMachineFixture("", "edge-a"),
          }),
        ],
      }),
    );

    expect(Result.isFailure(projected)).toBe(true);
  });
});
