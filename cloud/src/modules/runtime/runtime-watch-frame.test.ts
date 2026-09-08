import { describe, expect, it } from "vitest";
import { Option, Schema } from "effect";
import {
  runtimeSnapshotFromWatchFrame,
  runtimeWatchFrameForTransport,
  runtimeWatchFrameSchema,
} from "#/modules/runtime/runtime-watch-frame";
import {
  runtimeWatchCertificateFixture,
  runtimeWatchContainerFixture,
  runtimeWatchFrameFixture,
  runtimeWatchMachineFixture,
  runtimeWatchMachineObservationFixture,
  runtimeWatchVolumeFixture,
} from "#/modules/runtime/runtime-watch-frame.test-fixture";

const OBSERVED_AT = "2026-08-18T00:00:00.000Z";

describe("runtimeSnapshotFromWatchFrame", () => {
  it("retains direct Engine observations without inferring a runtime verdict", () => {
    const api = runtimeWatchContainerFixture("machine-a", "ctr-api");
    const hook = runtimeWatchContainerFixture("machine-b", "ctr-hook");
    const volume = runtimeWatchVolumeFixture("machine-a", "data");
    const certificate = runtimeWatchCertificateFixture("api.example.test", {
      status: "pending",
      last_error: "waiting for DNS",
      backoff: {
        failure_kind: "does_not_resolve",
        next_attempt_at: "2026-08-18T00:02:00.000Z",
        failures: 2,
      },
    });

    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      observed_at: OBSERVED_AT,
      hosted_dns_hostname: "brisk-river.up.ployz.app",
      machines: [
        runtimeWatchMachineObservationFixture({
          machine: runtimeWatchMachineFixture("machine-a", "edge-a", {
            public_ip: "203.0.113.10",
          }),
          membership: "suspect",
        }),
      ],
      containers: [api, hook],
      services: [
        {
          identity: "production/api",
          service_id: api.resolved_spec.service_id,
          containers: [api],
          hook_containers: [hook],
        },
      ],
      volumes: [volume],
      certificates: [certificate],
      incomplete_ids: {
        machines: ["machine-b" as typeof api.machine_id],
        containers: ["ctr-missing" as typeof api.container_id],
        volumes: [volume.id],
        certificates: [certificate.hostname],
      },
    }));
    const snapshot = runtimeSnapshotFromWatchFrame(frame);

    expect(frame).not.toHaveProperty("volumes");
    expect(frame.incomplete_ids.volumes).toEqual([
      { machine_id: "machine-a", name: "data" },
    ]);
    expect(snapshot).toEqual({
      status: "observed",
      error: null,
      hostedDnsHostname: "brisk-river.up.ployz.app",
      machines: [
        {
          id: "machine-a",
          name: "edge-a",
          publicIp: "203.0.113.10",
          endpoints: ["udp://203.0.113.10:51820"],
          membership: "suspect",
          observedContainerCount: 1,
          observedAt: OBSERVED_AT,
        },
      ],
      services: [
        {
          id: "production/api",
          identity: "production/api",
          serviceId: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          containers: [
            {
              id: "ctr-api",
              displayName: "ctr-api",
              machineId: "machine-a",
              projectName: "production",
              kind: "service_container",
            },
          ],
          hookContainers: [
            {
              id: "ctr-hook",
              displayName: "ctr-hook",
              machineId: "machine-b",
              projectName: "production",
              kind: "service_container",
            },
          ],
          observedAt: OBSERVED_AT,
        },
      ],
      certificates: [
        {
          hostname: "api.example.test",
          status: "pending",
          lastError: "waiting for DNS",
          backoff: {
            failureKind: "does_not_resolve",
            nextAttemptAt: "2026-08-18T00:02:00.000Z",
            failures: 2,
          },
        },
      ],
      incompleteIds: {
        machines: ["machine-b"],
        containers: ["ctr-missing"],
        volumes: [{ machineId: "machine-a", name: "data" }],
        certificates: ["api.example.test"],
      },
      observedAt: OBSERVED_AT,
    });
  });

  it("accepts additive SDK fields while requiring the retained evidence", () => {
    const frame = runtimeWatchFrameFixture({ observed_at: OBSERVED_AT });

    const additive = Schema.decodeUnknownOption(runtimeWatchFrameSchema)({
      ...runtimeWatchFrameForTransport(frame),
      future_runtime_field: { safe_to_ignore: true },
    });
    expect(Option.isSome(additive)).toBe(true);
    if (Option.isNone(additive)) throw new Error("Expected Runtime Watch frame.");
    expect(runtimeSnapshotFromWatchFrame(additive.value)).toMatchObject({
      status: "observed",
      observedAt: OBSERVED_AT,
    });

    expect(
      Option.isNone(
        Schema.decodeUnknownOption(runtimeWatchFrameSchema)({
          ...runtimeWatchFrameForTransport(frame),
          incomplete_ids: {
            ...frame.incomplete_ids,
            containers: "not-an-array",
          },
        }),
      ),
    ).toBe(true);
  });
});
