import type {
  ConfigMount,
  ConfigSpec,
  ContainerObservation,
  ContainerRuntimeObservation,
  DataLoss,
  DataLossConfirmation,
  DeployEvent,
  DeployIntent,
  DeviceMapping,
  DeviceReservation,
  HealthcheckSpec,
  MachineId,
  ProjectName,
  RequestedServiceSpec,
  RestartPolicy,
  ResolvedServiceSpec,
  ServiceMode,
  ServiceName,
  Ulimit,
  VolumeDriver,
} from "../generated/payloads";
import { RpcError } from "../index";

new RpcError({
  code: "unavailable",
  message: "Watch interrupted",
}) satisfies Error;

({ name: "nfs", options: { share: "app" } }) satisfies VolumeDriver;
([{ name: "settings", content: [112, 111, 114, 116] }]) satisfies NonNullable<
  RequestedServiceSpec["configs"]
>;
([{ name: "settings", content: [112, 111, 114, 116] }]) satisfies NonNullable<
  ResolvedServiceSpec["configs"]
>;
({
  config_name: "settings",
  target: "/etc/api/settings.toml",
  uid: 1000,
  gid: 1000,
  mode: 0o440,
}) satisfies ConfigMount;
({
  machine_path: "/dev/fuse",
  container_path: "/dev/fuse",
  cgroup_permissions: "rwm",
}) satisfies DeviceMapping;
({
  driver: "nvidia",
  count: 1,
  device_ids: ["GPU-0"],
  capabilities: [["gpu"]],
  options: { count: "1" },
}) satisfies DeviceReservation;
({ soft: 1024, hard: 2048 }) satisfies Ulimit;
({ state: "configured", test: ["CMD", "true"] }) satisfies HealthcheckSpec;
({ state: "disabled" }) satisfies ContainerObservation["effective_healthcheck"];

// @ts-expect-error VolumeDriver options values are strings
const invalidDriver: VolumeDriver = { name: "nfs", options: { share: 1 } };
// @ts-expect-error DeviceMapping requires cgroup_permissions
const invalidDevice: DeviceMapping = {
  machine_path: "/dev/fuse",
  container_path: "/dev/fuse",
};
// @ts-expect-error ConfigSpec content is a byte array
const invalidConfig: ConfigSpec = { name: "settings", content: "port = 8080" };
// @ts-expect-error Ulimit requires hard
const invalidUlimit: Ulimit = { soft: 1024 };
// @ts-expect-error DeviceReservation count is a number
const invalidReservation: DeviceReservation = { count: "one" };
// @ts-expect-error HealthcheckSpec is tagged, not a string
const invalidHealthcheck: HealthcheckSpec = "disabled";
// @ts-expect-error configs is ConfigSpec[], not a number
const invalidConfigs: RequestedServiceSpec["configs"] = 1;
// @ts-expect-error effective_healthcheck is HealthcheckSpec | null, not a string
const invalidEffective: ContainerObservation["effective_healthcheck"] =
  "disabled";
// @ts-expect-error DataLossConfirmation is an object, not a bare Data Loss list
const invalidConfirmation: DataLossConfirmation = [];

// Payloads are plain object types, not index-signature intersections, so a
// literal with a misspelled field is rejected instead of absorbed.
const web: RequestedServiceSpec = {
  name: "web" as ServiceName,
  mode: { mode: "replicated", replicas: 1 },
  container: { image: "nginx", pull_policy: "always" },
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
({
  name: "web" as ServiceName,
  // @ts-expect-error replica is not a field of the replicated ServiceMode arm
  mode: { mode: "replicated", replica: 1 },
  container: { image: "nginx", pull_policy: "always" },
}) satisfies RequestedServiceSpec;
({
  project_name: "app" as ProjectName,
  target: [web],
  options: intent.options,
  // @ts-expect-error targets is not a field of DeployIntent
  targets: [web],
}) satisfies DeployIntent;
// keyof a payload is its declared field names, not string.
("project_name") satisfies keyof DeployIntent;
// @ts-expect-error an undeclared name is not a key of DeployIntent
("from_a_newer_daemon") satisfies keyof DeployIntent;
// @ts-expect-error unknown keys are not readable on a plain object type
const unknownField: unknown = intent.from_a_newer_daemon;

// Tagged unions are closed: Rust rejects an unknown tag, and the one state
// Rust passes through is a named arm. So `switch` narrows and exhausts.
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
// @ts-expect-error a HealthcheckSpec needs a known state
const noHealthcheck: HealthcheckSpec = {};
// @ts-expect-error an unknown Docker state is not a bare tag; it arrives as unrecognized + raw
const futureState: ContainerRuntimeObservation = { state: "hibernating" };

// Data Loss is a tagged union whose identity nests per kind.
// @ts-expect-error identity fields do not spread beside the kind
const flatLoss: DataLoss = { kind: "docker_volume", machine_id: "m" as MachineId, name: "data" };
