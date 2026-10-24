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

  it("keeps the mark of a run its Server stopped on request beside its exit code", () => {
    const stopped = runtimeWatchContainerFixture("machine-a", "ctr-stopped");
    stopped.runtime = { state: "exited", code: 1 };
    stopped.stopped = true;
    const crashed = runtimeWatchContainerFixture("machine-a", "ctr-crashed");
    crashed.runtime = { state: "exited", code: 1 };
    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      services: [{ identity: "production/api", service_id: stopped.resolved_spec.service_id, containers: [stopped, crashed], hook_containers: [] }],
      containers: [stopped, crashed],
    }));
    const parsed = Schema.decodeUnknownSync(runtimeWatchFrameSchema)(frame);
    expect(runtimeSnapshotFromWatchFrame(parsed).services[0]?.containers.map((container) => container.runtime)).toEqual([
      { state: "exited", code: 1, stopped: true },
      { state: "exited", code: 1 },
    ]);
  });

  it("lets the browser read a frame whose runtime carries a key its bundle does not know, as an older bundle reads stopped", () => {
    const container = runtimeWatchContainerFixture("machine-a", "ctr-stopped");
    container.runtime = { state: "exited", code: 1 };
    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      services: [{ identity: "production/api", service_id: container.resolved_spec.service_id, containers: [container], hook_containers: [] }],
      containers: [container],
    }));
    const newer = { ...frame, containers: frame.containers.map((sent) => ({ ...sent, runtime: { ...sent.runtime, unknown_to_this_bundle: true } })) };
    const parsed = Schema.decodeUnknownOption(runtimeWatchFrameSchema)(newer);
    expect(Option.isSome(parsed)).toBe(true);
    expect(Option.getOrThrow(parsed).containers[0]?.runtime).toEqual({ state: "exited", code: 1 });
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
              pool_committable_bytes: 12_884_901_888,
              capabilities: ["ployz.logs.tail.v1"],
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
          capabilities: ["ployz.logs.tail.v1"],
          poolCommittableBytes: 12_884_901_888,
          sealing: null,
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
      published: [],
      observedAt: OBSERVED_AT,
    });
  });

  it("lists the hostnames Service containers publish through ingress, certificate or not", () => {
    const withPorts = (id: string, kind: "service_container" | "pre_deploy_hook", ports: ReturnType<typeof runtimeWatchContainerFixture>["resolved_spec"]["ports"]) => {
      const container = runtimeWatchContainerFixture("machine-a", id);
      return { ...container, kind, resolved_spec: { ...container.resolved_spec, ports } };
    };
    const ingress = (hostname: string) => ({ mode: "ingress" as const, hostname, load_balancer_port: 80, container_port: 8080, http_protocol: "http" as const });
    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      containers: [
        withPorts("ctr-api-1", "service_container", [ingress("api.example.test"), { mode: "host", bind: { kind: "all" }, published_port: 5432, container_port: 5432, transport_protocol: "tcp" }]),
        withPorts("ctr-api-2", "service_container", [ingress("api.example.test"), ingress("plain.example.test")]),
        withPorts("ctr-hook", "pre_deploy_hook", [ingress("hook.example.test")]),
      ],
      certificates: [runtimeWatchCertificateFixture("*.acme.ployz.app")],
    }));

    expect(runtimeSnapshotFromWatchFrame(frame).published).toEqual(["api.example.test", "plain.example.test"]);
  });

  it("carries each managed Volume copy's role and bound, and a slot's role as its Machine records it, and nothing else of the Volume", () => {
    const provisioned = (machineId: string, role: "slot" | "switching" | null, slot_role?: "mirror" | "old_copy") =>
      runtimeWatchVolumeFixture(machineId, "app_vol-v1", {
        labels: { secret: "label" },
        storage: { kind: "provisioned", mountpoint: "/var/lib/ployz/volumes/app_vol-v1", bound_bytes: 10, used_bytes: 1, role, slot_role },
      });
    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      volumes: [
        provisioned("web-1", null),
        provisioned("web-2", "slot", "mirror"),
        provisioned("web-3", "switching"),
        provisioned("web-4", "slot", "old_copy"),
        provisioned("web-5", "slot"),
        runtimeWatchVolumeFixture("web-1", "plain"),
      ],
    }));

    expect(frame.volumes).toEqual([
      { machine_id: "web-1", name: "app_vol-v1", role: "writer", slot_role: null, bound_bytes: 10 },
      { machine_id: "web-2", name: "app_vol-v1", role: "slot", slot_role: "mirror", bound_bytes: 10 },
      { machine_id: "web-3", name: "app_vol-v1", role: "switching", slot_role: null, bound_bytes: 10 },
      { machine_id: "web-4", name: "app_vol-v1", role: "slot", slot_role: "old_copy", bound_bytes: 10 },
      { machine_id: "web-5", name: "app_vol-v1", role: "slot", slot_role: "mirror", bound_bytes: 10 },
    ]);
    expect(runtimeSnapshotFromWatchFrame(frame).volumeCopies.map(({ machineId, role, slotRole, boundBytes }) => [machineId, role, slotRole, boundBytes])).toEqual([
      ["web-1", "writer", null, 10],
      ["web-2", "slot", "mirror", 10],
      ["web-3", "switching", null, 10],
      ["web-4", "slot", "old_copy", 10],
      ["web-5", "slot", "mirror", 10],
    ]);
  });

  it("reads a copy's bound as unknown from a transport from before it carried one", () => {
    const frame = Schema.decodeUnknownSync(runtimeWatchFrameSchema)({
      ...runtimeWatchFrameForTransport(runtimeWatchFrameFixture()),
      volumes: [{ machine_id: "web-1", name: "app_vol-v1", role: "writer", slot_role: null }],
    });
    expect(runtimeSnapshotFromWatchFrame(frame).volumeCopies[0]?.boundBytes).toBeNull();
  });

  it("reads a Pool's room as unknown from a daemon that doesn't report it, or a transport from before it did", () => {
    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      machines: [runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture("machine-a", "edge-a") })],
    }));
    expect(runtimeSnapshotFromWatchFrame(frame).machines[0]?.poolCommittableBytes).toBeNull();

    const [machine] = frame.machines;
    if (!machine) throw new Error("Expected a machine.");
    const { pool_committable_bytes: _, ...olderRuntime } = machine.machine.runtime;
    const older = Schema.decodeUnknownOption(runtimeWatchFrameSchema)({
      ...frame,
      machines: [{ ...machine, machine: { ...machine.machine, runtime: olderRuntime } }],
    });
    if (Option.isNone(older)) throw new Error("Expected Runtime Watch frame.");
    expect(runtimeSnapshotFromWatchFrame(older.value).machines[0]?.poolCommittableBytes).toBeNull();
  });

  it("reads whether a Server holds its Cluster's Sealing Key or awaits its recovery, and as unknown from a daemon that doesn't report it", () => {
    const base = runtimeWatchMachineFixture("machine-a", "edge-a");
    const needsAccess = { ...base.runtime, sealing: { kind: "needs_access" } as const };
    const awaitsRecovery = { kind: "awaits_recovery", public_key: `${"A".repeat(43)}=` } as const;
    const awaiting = runtimeWatchMachineFixture("machine-c", "edge-c");
    const frame = runtimeWatchFrameForTransport(runtimeWatchFrameFixture({
      machines: [
        runtimeWatchMachineObservationFixture({ machine: { ...base, runtime: needsAccess } }),
        runtimeWatchMachineObservationFixture({ machine: runtimeWatchMachineFixture("machine-b", "edge-b") }),
        runtimeWatchMachineObservationFixture({ machine: { ...awaiting, runtime: { ...awaiting.runtime, sealing: awaitsRecovery } } }),
      ],
    }));
    expect(runtimeSnapshotFromWatchFrame(frame).machines.map((machine) => machine.sealing)).toEqual([{ kind: "needs_access" }, null, awaitsRecovery]);

    const [machine] = frame.machines;
    if (!machine) throw new Error("Expected a machine.");
    const { sealing: _, ...olderRuntime } = machine.machine.runtime;
    const older = Schema.decodeUnknownOption(runtimeWatchFrameSchema)({
      ...frame,
      machines: [{ ...machine, machine: { ...machine.machine, runtime: olderRuntime } }],
    });
    if (Option.isNone(older)) throw new Error("Expected Runtime Watch frame.");
    expect(runtimeSnapshotFromWatchFrame(older.value).machines[0]?.sealing).toBeNull();
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
