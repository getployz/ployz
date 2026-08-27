import type {
  ConfigMount,
  ConfigSpec,
  ContainerObservation,
  DataLossConfirmation,
  DeviceMapping,
  DeviceReservation,
  HealthcheckSpec,
  RequestedServiceSpec,
  ResolvedServiceSpec,
  Ulimit,
  VolumeDriver,
} from "../generated/payloads";

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
