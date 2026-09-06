// Compile-time checks over the generated declarations and the hand-written
// façade. `tsc --noEmit` over this file is the TypeScript-side guard.
import type {
  ClusterTeardown,
  ConfigMount,
  ConfigSpec,
  ContainerObservation,
  ContainerRuntimeObservation,
  DataLoss,
  DataLossConfirmation,
  DeployEvent,
  DeployIntent,
  DeviceMapping,
  HealthcheckSpec,
  LocalMachineRemoved,
  MachineId,
  ProjectName,
  RegisterRequest,
  Registered,
  RequestedServiceSpec,
  ResolvedVolumeSource,
  RestartPolicy,
  RuntimeWatchFrame,
  ServiceContainerSpec,
  ServiceMode,
  ServiceName,
  ServiceObservation,
  Ulimit,
  VolumeSource,
} from "../generated/payloads";
import {
  applyAll,
  applyOne,
  Client,
  connect,
  listHeld,
  register,
  RpcError,
} from "../index";
import type { HeldRegister, PreparedDeploy } from "../index";

// Every field serde always writes is present in the type; `Option` is `T | null`.
const container: ServiceContainerSpec = {
  config_mounts: [],
  image: "nginx",
  command: [],
  entrypoint: [],
  environment: {},
  labels: {},
  hostname: null,
  extra_hosts: [],
  cap_add: [],
  cap_drop: [],
  healthcheck: null,
  pull_policy: "always",
  init: null,
  user: null,
  working_directory: null,
  tty: false,
  open_stdin: false,
  privileged: false,
  pid_mode: null,
  log_driver: null,
  resources: {
    cpu_nanos: null,
    memory_bytes: null,
    memory_reservation_bytes: null,
    shared_memory_bytes: null,
    devices: [],
    device_reservations: [],
    ulimits: {},
  },
  stop_timeout_secs: null,
  sysctls: {},
  restart: { name: "always" },
};
const web: RequestedServiceSpec = {
  name: "web" as ServiceName,
  mode: { mode: "replicated", replicas: 1 },
  container,
  placement: { machines: [] },
  ports: [],
  volumes: [],
  mounts: [],
  configs: [],
  pre_deploy: null,
  ingress_proxy_fragment: null,
  update: { order: null, monitor_millis: null },
};
const intent: DeployIntent = {
  project_name: "app" as ProjectName,
  target: [web],
  options: {
    force_recreate: false,
    skip_health_monitor: false,
    placement_seed: 0,
    selected: [{ name: "web" as ServiceName }],
  },
};

new RpcError({ code: "unavailable", message: "Watch interrupted", details: null }) satisfies Error;
([{ name: "settings", content: [112, 111, 114, 116] }]) satisfies RequestedServiceSpec["configs"];
({ config_name: "settings", target: "/etc/api/settings.toml", uid: 1000, gid: 1000, mode: 0o440 }) satisfies ConfigMount;
({ machine_path: "/dev/fuse", container_path: "/dev/fuse", cgroup_permissions: "rwm" }) satisfies DeviceMapping;
({ soft: 1024, hard: 2048 }) satisfies Ulimit;
({ state: "disabled" }) satisfies HealthcheckSpec;
({ state: "disabled" }) satisfies ContainerObservation["effective_healthcheck"];

// @ts-expect-error DeviceMapping requires cgroup_permissions
const invalidDevice: DeviceMapping = { machine_path: "/dev/fuse", container_path: "/dev/fuse" };
// @ts-expect-error ConfigSpec content is a byte array
const invalidConfig: ConfigSpec = { name: "settings", content: "port = 8080" };
// @ts-expect-error Ulimit requires hard
const invalidUlimit: Ulimit = { soft: 1024 };
// @ts-expect-error HealthcheckSpec is tagged, not a string
const invalidHealthcheck: HealthcheckSpec = "disabled";
// @ts-expect-error effective_healthcheck is HealthcheckSpec | null, not a string
const invalidEffective: ContainerObservation["effective_healthcheck"] = "disabled";
// @ts-expect-error DataLossConfirmation is an object, not a bare Data Loss list
const invalidConfirmation: DataLossConfirmation = [];

// Payloads are plain object types, so a misspelled field is rejected, not absorbed.
({
  ...web,
  // @ts-expect-error replica is not a field of the replicated ServiceMode arm
  mode: { mode: "replicated", replica: 1 },
}) satisfies RequestedServiceSpec;
({
  ...intent,
  // @ts-expect-error targets is not a field of DeployIntent
  targets: [web],
}) satisfies DeployIntent;
("project_name") satisfies keyof DeployIntent;
// @ts-expect-error an undeclared name is not a key of DeployIntent
("from_a_newer_daemon") satisfies keyof DeployIntent;

// Tagged unions are closed on the wire: an unknown Docker state arrives as the
// `unrecognized` arm carrying the observed value, so `switch` exhausts.
function describeRuntime(runtime: ContainerRuntimeObservation): string {
  switch (runtime.state) {
    case "running":
      return runtime.health;
    case "exited":
      return String(runtime.code);
    case "unrecognized":
      return JSON.stringify(runtime.raw);
    case "created":
    case "paused":
    case "restarting":
    case "removing":
    case "dead":
      return runtime.state;
    default: {
      const exhaustive: never = runtime;
      return exhaustive;
    }
  }
}
void describeRuntime;
// @ts-expect-error the empty object is not a DeployEvent
const noEvent: DeployEvent = {};
// @ts-expect-error a ServiceMode needs a known mode
const noMode: ServiceMode = {};
// @ts-expect-error a RestartPolicy needs a known name
const noRestart: RestartPolicy = {};
// @ts-expect-error an unknown Docker state is not a bare tag; it arrives as unrecognized + raw
const futureState: ContainerRuntimeObservation = { state: "hibernating" };

// Data Loss identity nests per kind.
// @ts-expect-error identity fields do not spread beside the kind
const flatLoss: DataLoss = { kind: "docker_volume", machine_id: "m" as MachineId, name: "data" };

const ordinary = { kind: "ordinary", name: "data", driver: { name: "local", options: {} }, labels: {} } as const;
ordinary satisfies VolumeSource;
({ ...ordinary, scope: { project: "app" as ProjectName, logical_name: "data" } }) satisfies ResolvedVolumeSource;
// @ts-expect-error a resolved source always states its scope, even when absent
ordinary satisfies ResolvedVolumeSource;

// The façade accepts generated payloads and keeps destructive actions explicit.
declare const client: Client;
const connectOptions = {
  relayUrl: "https://relay.example",
  bearer: "bearer",
  pairing: "pairing",
  machineId: "machine" as MachineId,
};
connect(connectOptions) satisfies Promise<Client>;
listHeld("https://relay.example", "bearer", "pairing") satisfies Promise<HeldRegister[]>;
const identity: RegisterRequest = {
  name: "machine",
  storage: "none",
  public_key: [],
  public_ip: null,
  advertised_endpoints: [],
  runtime: {
    daemon_version: "1",
    docker_version: "1",
    hostname: "machine",
    architecture: "arm64",
    os_pretty_name: "macOS",
    kernel_version: "1",
  },
};
register("https://relay.example", "bearer", "pairing", "machine" as MachineId, identity) satisfies Promise<Registered>;
applyAll("app" as ProjectName, [web]) satisfies DeployIntent;
applyOne("app" as ProjectName, web) satisfies DeployIntent;
client.preview(intent) satisfies Promise<PreparedDeploy>;
client.runtime.watch() satisfies AsyncIterable<RuntimeWatchFrame>;
client.removeMachine("machine", { confirmed: [] }) satisfies Promise<LocalMachineRemoved>;
client.destroyCluster({ confirmed: [] }) satisfies Promise<ClusterTeardown>;
// @ts-expect-error destructive methods require an explicit confirmation object
client.removeMachine("machine", []);
// @ts-expect-error MachineId is branded; a plain string cannot cross the façade
connect({ ...connectOptions, machineId: "machine" });

declare const watchFrame: RuntimeWatchFrame;
watchFrame.services satisfies ServiceObservation[];
