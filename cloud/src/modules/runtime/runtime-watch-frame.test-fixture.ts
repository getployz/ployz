import type {
  CertificateObservation,
  ContainerId,
  ContainerObservation,
  DockerVolume,
  Machine,
  MachineId,
  MachineObservation,
  ResolvedServiceSpec,
  RuntimeWatchView,
  ServiceId,
} from "@ployz/sdk";

export function runtimeWatchFrameFixture(
  overrides: Partial<RuntimeWatchView> = {},
): RuntimeWatchView {
  return {
    hosted_dns_hostname: null,
    machines: [],
    containers: [],
    services: [],
    volumes: [],
    certificates: [],
    incomplete_ids: {
      machines: [],
      containers: [],
      volumes: [],
      certificates: [],
    },
    observed_at: "2026-08-18T00:00:00.000Z",
    ...overrides,
  };
}

export function runtimeWatchMachineFixture(
  id: string,
  name: string,
  extra: Partial<Machine> = {},
): Machine {
  return {
    id: id as MachineId,
    name,
    subnet: "10.0.0.0/24",
    public_ip: null,
    public_key: [1],
    labels: {},
    accepts_builds: true,
    accepts_services: true,
    accepts_ingress: true,
    advertised_endpoints: ["udp://203.0.113.10:51820"],
    runtime: {
      daemon_version: "0.1.2",
      docker_version: "27.0.0",
      hostname: name,
      architecture: "x86_64",
      os_pretty_name: "Debian",
      kernel_version: "6.1.0",
    },
    ...extra,
  };
}

export function runtimeWatchMachineObservationFixture(
  extra: Partial<MachineObservation> & { machine: Machine },
): MachineObservation {
  return {
    storage: null,
    rtt: null,
    membership: "up",
    selected_endpoint: null,
    ...extra,
  };
}

export function runtimeWatchContainerFixture(
  machineId: string,
  containerId: string,
): ContainerObservation {
  return {
    container_id: containerId as ContainerId,
    display_name: containerId,
    created_at_unix_nanos: 0,
    machine_id: machineId as MachineId,
    project_name: "production",
    kind: "service_container",
    runtime: { state: "running", health: "healthy" },
    effective_healthcheck: null,
    resolved_spec: resolvedServiceSpecFixture(),
    address: null,
    labels: {},
  };
}

export function runtimeWatchVolumeFixture(
  machineId: string,
  name: string,
  extra: Partial<DockerVolume> = {},
): DockerVolume {
  return {
    id: { machine_id: machineId as MachineId, name },
    options: {},
    labels: {},
    storage: { kind: "plain", driver: "local" },
    ...extra,
  };
}

export function runtimeWatchCertificateFixture(
  hostname: string,
  extra: Partial<CertificateObservation> = {},
): CertificateObservation {
  return {
    hostname,
    status: "unknown",
    last_error: null,
    backoff: null,
    ...extra,
  };
}

export function resolvedServiceSpecFixture(): ResolvedServiceSpec {
  return {
    service_id: "a".repeat(32) as ServiceId,
    name: "api",
    mode: { mode: "replicated", replicas: 1 },
    placement: { constraints: [] },
    ports: [], volumes: [], mounts: [], configs: [],
    pre_deploy: null, ingress_proxy_fragment: null,
    update: { order: "start_first", monitor_millis: null },
    container: {
      image: "nginx:1.27", pull_policy: "missing",
      command: [], entrypoint: [], environment: {}, config_mounts: [],
      labels: {}, hostname: null, extra_hosts: [], cap_add: [], cap_drop: [],
      healthcheck: null, init: null, user: null, working_directory: null,
      tty: false, open_stdin: false, privileged: false, pid_mode: null,
      log_driver: null, stop_timeout_secs: null, sysctls: {},
      restart: { name: "unless-stopped" },
      resources: {
        cpu_nanos: null, memory_bytes: null, memory_reservation_bytes: null,
        shared_memory_bytes: null, devices: [], device_reservations: [], ulimits: {},
      },
    },
  };
}
