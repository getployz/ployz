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
  it("retains exit codes across the redacted watch and browser snapshot", () => {
    const container = runtimeWatchContainerFixture("machine-a", "ctr-stopped");
    container.runtime = { state: "exited", code: 0 };
    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      services: [{ identity: "production/api", service_id: container.resolved_spec.service_id, containers: [container], hook_containers: [] }],
      containers: [container],
    }));
    const parsed = Schema.decodeUnknownSync(runtimeWatchFrameSchema)(frame);
    expect(parsed.containers[0]?.runtime).toEqual({ state: "exited", code: 0 });
    expect(runtimeSnapshotFromWatchFrame(parsed).services[0]?.containers[0]?.runtime).toEqual({ state: "exited", code: 0 });
    expect(frame.containers[0]).not.toHaveProperty("resolved_spec");
  });

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
      via_proxy: true,
    });

    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      observed_at: OBSERVED_AT,
      effective_build_concurrency: { ["machine-a" as typeof api.machine_id]: 2 },
      machines: [
        runtimeWatchMachineObservationFixture({
          machine: runtimeWatchMachineFixture("machine-a", "edge-a", {
            public_ip: "203.0.113.10",
            accepts_services: false,
            build_concurrency: 2,
            runtime: {
              daemon_version: "0.1.2",
              docker_version: "27.0.0",
              hostname: "edge-a",
              architecture: "x86_64",
              os_pretty_name: "Debian",
              kernel_version: "6.1.0",
              memory_total_bytes: 16_000_000_000,
              running_builds: 1,
            },
          }),
          membership: "suspect",
          storage: { state: "pool", size_bytes: 20_000_000_000, used_bytes: 1_000_000_000, free_bytes: 19_000_000_000 },
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

    expect(frame.volumes).toEqual([]);
    expect(frame.incomplete_ids.volumes).toEqual([
      { machine_id: "machine-a", name: "data" },
    ]);
    expect(snapshot).toEqual({
      status: "observed",
      error: null,
      machines: [
        {
          id: "machine-a",
          name: "edge-a",
          publicIp: "203.0.113.10",
          acceptsIngress: true,
          endpoints: ["udp://203.0.113.10:51820"],
          acceptsBuilds: true,
          acceptsServices: false,
          storage: "pool",
          buildConcurrency: 2,
          effectiveBuildConcurrency: 2,
          runningBuilds: 1,
          daemonVersion: "0.1.2",
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
              namespace: "production",
              kind: "service_container",
              runtime: { state: "running", health: "healthy" },
            },
          ],
          hookContainers: [
            {
              id: "ctr-hook",
              displayName: "ctr-hook",
              machineId: "machine-b",
              namespace: "production",
              kind: "service_container",
              runtime: { state: "running", health: "healthy" },
            },
          ],
          observedAt: OBSERVED_AT,
        },
      ],
      volumeCopies: [],
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
          viaProxy: true,
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

  it("carries each managed Volume copy's role and nothing else of the Volume", () => {
    const provisioned = (machineId: string, role: "slot" | "switching" | null) => runtimeWatchVolumeFixture(machineId, "app_vol-v1", {
      labels: { secret: "label" },
      storage: { kind: "provisioned", mountpoint: "/var/lib/ployz/volumes/app_vol-v1", bound_bytes: 10, used_bytes: 1, role },
    });
    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      volumes: [provisioned("web-1", null), provisioned("web-2", "slot"), provisioned("web-3", "switching"), runtimeWatchVolumeFixture("web-1", "plain")],
    }));

    expect(frame.volumes).toEqual([
      { machine_id: "web-1", name: "app_vol-v1", role: "writer" },
      { machine_id: "web-2", name: "app_vol-v1", role: "slot" },
      { machine_id: "web-3", name: "app_vol-v1", role: "switching" },
    ]);
    expect(runtimeSnapshotFromWatchFrame(frame).volumeCopies).toEqual([
      { machineId: "web-1", name: "app_vol-v1", role: "writer" },
      { machineId: "web-2", name: "app_vol-v1", role: "slot" },
      { machineId: "web-3", name: "app_vol-v1", role: "switching" },
    ]);
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
