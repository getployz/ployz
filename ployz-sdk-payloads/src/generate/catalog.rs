//! Shape catalog for generated `@ployz/sdk` TypeScript.

use ployz_core::{
    CertificateAvailability, CertificateFailureKind, HealthObservation, MembershipObservation,
    RpcErrorCode,
};

pub(super) enum Shape {
    Alias(&'static str),
    Branded,
    OpenString(&'static [&'static str]),
    ClosedString(&'static [&'static str]),
    Object {
        params: &'static str,
        fields: &'static [(&'static str, &'static str)],
    },
    InternallyTagged {
        tag: &'static str,
        params: &'static str,
        variants: &'static [(&'static str, &'static [(&'static str, &'static str)])],
    },
}

pub(super) const PAYLOADS: &[(&str, Shape)] = &[
    ("CpuNanos", Shape::Alias("number")),
    ("ByteQuantity", Shape::Alias("number")),
    ("MachineId", Shape::Branded),
    ("ContainerId", Shape::Branded),
    ("ServiceId", Shape::Branded),
    ("ServiceName", Shape::Branded),
    ("ProjectName", Shape::Branded),
    ("QualifiedService", Shape::Alias("string")),
    ("MachineName", Shape::Alias("string")),
    ("MachineSubnet", Shape::Alias("string")),
    ("ManagementAddress", Shape::Alias("string")),
    ("AdvertisedEndpoint", Shape::Alias("string")),
    ("SelectedEndpoint", Shape::Alias("string")),
    ("ContainerAddress", Shape::Alias("string")),
    ("IngressHost", Shape::Alias("string")),
    ("ContainerHostname", Shape::Alias("string")),
    (
        "ContainerLabels",
        Shape::Alias("{ readonly [key: string]: string }"),
    ),
    ("ExtraHost", Shape::Alias("string")),
    ("DockerVolumeName", Shape::Alias("string")),
    ("CapabilityName", Shape::Alias("string")),
    ("WireGuardPublicKey", Shape::Alias("number[]")),
    ("MachinePath", Shape::Alias("string")),
    ("ContainerPath", Shape::Alias("string")),
    ("ServiceVolumeReference", Shape::Alias("string")),
    ("ProvisionedVolumeMaximumBytes", Shape::Alias("string")),
    ("MachineTarget", Shape::Alias("string")),
    ("PidMode", Shape::Alias("string")),
    (
        "PullPolicy",
        Shape::ClosedString(&["always", "missing", "never"]),
    ),
    ("StorageChoice", Shape::ClosedString(&["none", "zfs"])),
    (
        "MachineStorageObservation",
        Shape::InternallyTagged {
            tag: "state",
            params: "",
            variants: &[
                ("stateless", &[]),
                ("ready", &[]),
                (
                    "pool",
                    &[
                        ("size_bytes", "number"),
                        ("used_bytes", "number"),
                        ("free_bytes", "number"),
                    ],
                ),
            ],
        },
    ),
    (
        "UpdateOrder",
        Shape::ClosedString(&["start_first", "stop_first"]),
    ),
    ("HttpProtocol", Shape::ClosedString(&["http", "https"])),
    ("TransportProtocol", Shape::ClosedString(&["tcp", "udp"])),
    (
        "ServiceMode",
        Shape::InternallyTagged {
            tag: "mode",
            params: "",
            variants: &[("replicated", &[("replicas", "number")]), ("global", &[])],
        },
    ),
    ("IngressProxyFragment", Shape::Alias("string")),
    (
        "IngressProxyConfig",
        Shape::Object {
            params: "",
            fields: &[("config", "string")],
        },
    ),
    (
        "IngressHostname",
        Shape::InternallyTagged {
            tag: "kind",
            params: "",
            variants: &[
                ("cluster_domain", &[("label", "string?")]),
                ("explicit", &[("hostname", "IngressHost")]),
            ],
        },
    ),
    (
        "HostBind",
        Shape::InternallyTagged {
            tag: "kind",
            params: "",
            variants: &[
                ("all", &[]),
                ("address", &[("address", "string")]),
                ("prefix", &[("prefix", "string")]),
            ],
        },
    ),
    (
        "PortPublication",
        Shape::InternallyTagged {
            tag: "mode",
            params: "",
            variants: &[
                (
                    "ingress",
                    &[
                        ("hostname", "IngressHostname"),
                        ("load_balancer_port", "number"),
                        ("container_port", "number"),
                        ("http_protocol", "HttpProtocol"),
                    ],
                ),
                (
                    "host",
                    &[
                        ("bind", "HostBind"),
                        ("published_port", "number"),
                        ("container_port", "number"),
                        ("transport_protocol", "TransportProtocol"),
                    ],
                ),
            ],
        },
    ),
    (
        "VolumeDriver",
        Shape::Object {
            params: "",
            fields: &[
                ("name", "string"),
                ("options", "{ readonly [key: string]: string }"),
            ],
        },
    ),
    (
        "VolumeSource",
        Shape::InternallyTagged {
            tag: "kind",
            params: "",
            variants: &[
                (
                    "bind",
                    &[
                        ("machine_path", "MachinePath"),
                        ("create_machine_path", "boolean?"),
                        ("propagation", "string?"),
                        ("recursive", "string?"),
                    ],
                ),
                ("external", &[("name", "DockerVolumeName")]),
                (
                    "ordinary",
                    &[
                        ("name", "DockerVolumeName"),
                        ("driver", "VolumeDriver"),
                        ("labels", "{ readonly [key: string]: string }?"),
                    ],
                ),
                (
                    "provisioned",
                    &[
                        ("name", "DockerVolumeName"),
                        ("maximum_bytes", "ProvisionedVolumeMaximumBytes"),
                        ("labels", "{ readonly [key: string]: string }?"),
                    ],
                ),
                (
                    "tmpfs",
                    &[
                        ("size_bytes", "number?"),
                        ("mode", "number?"),
                        ("options", "string[][]?"),
                    ],
                ),
            ],
        },
    ),
    (
        "HealthcheckSpec",
        Shape::InternallyTagged {
            tag: "state",
            params: "",
            variants: &[
                ("disabled", &[]),
                (
                    "configured",
                    &[
                        ("test", "string[]"),
                        ("interval_millis", "number?"),
                        ("timeout_millis", "number?"),
                        ("start_period_millis", "number?"),
                        ("start_interval_millis", "number?"),
                        ("retries", "number?"),
                    ],
                ),
            ],
        },
    ),
    (
        "RestartPolicy",
        Shape::InternallyTagged {
            tag: "name",
            params: "",
            variants: &[
                ("no", &[]),
                ("always", &[]),
                ("unless-stopped", &[]),
                ("on-failure", &[("maximum_retry_count", "number?")]),
            ],
        },
    ),
    (
        "LogDriver",
        Shape::Object {
            params: "",
            fields: &[
                ("name", "string"),
                ("options", "{ readonly [key: string]: string }"),
            ],
        },
    ),
    (
        "DeviceMapping",
        Shape::Object {
            params: "",
            fields: &[
                ("machine_path", "MachinePath"),
                ("container_path", "ContainerPath"),
                ("cgroup_permissions", "string"),
            ],
        },
    ),
    (
        "DeviceReservation",
        Shape::Object {
            params: "",
            fields: &[
                ("driver", "string?"),
                ("count", "number?"),
                ("device_ids", "string[]?"),
                ("capabilities", "string[][]?"),
                ("options", "{ readonly [key: string]: string }?"),
            ],
        },
    ),
    (
        "Ulimit",
        Shape::Object {
            params: "",
            fields: &[("soft", "number"), ("hard", "number")],
        },
    ),
    (
        "ContainerResources",
        Shape::Object {
            params: "",
            fields: &[
                ("cpu_nanos", "CpuNanos?"),
                ("memory_bytes", "ByteQuantity?"),
                ("memory_reservation_bytes", "ByteQuantity?"),
                ("shared_memory_bytes", "ByteQuantity?"),
                ("devices", "DeviceMapping[]?"),
                ("device_reservations", "DeviceReservation[]?"),
                ("ulimits", "{ readonly [key: string]: Ulimit }?"),
            ],
        },
    ),
    (
        "UpdateConfig",
        Shape::Object {
            params: "",
            fields: &[("order", "UpdateOrder?"), ("monitor_millis", "number?")],
        },
    ),
    (
        "ResolvedUpdateConfig",
        Shape::Object {
            params: "",
            fields: &[("order", "UpdateOrder"), ("monitor_millis", "number?")],
        },
    ),
    (
        "Placement",
        Shape::Object {
            params: "",
            fields: &[("machines", "MachineTarget[]?")],
        },
    ),
    (
        "PreDeployHook",
        Shape::Object {
            params: "",
            fields: &[
                ("command", "readonly [string, ...string[]]"),
                ("environment", "{ readonly [key: string]: string }?"),
                ("privileged", "boolean?"),
                ("timeout_millis", "number?"),
                ("user", "string?"),
            ],
        },
    ),
    (
        "ServiceMount",
        Shape::Object {
            params: "",
            fields: &[
                ("volume", "ServiceVolumeReference"),
                ("target", "ContainerPath"),
                ("read_only", "boolean?"),
                ("no_copy", "boolean?"),
                ("subpath", "string?"),
            ],
        },
    ),
    (
        "ServiceVolume",
        Shape::Object {
            params: "",
            fields: &[
                ("reference", "ServiceVolumeReference"),
                ("source", "VolumeSource"),
            ],
        },
    ),
    (
        "ScopedVolumeSource",
        Shape::Object {
            params: "",
            fields: &[
                ("project", "ProjectName"),
                ("logical_name", "DockerVolumeName"),
            ],
        },
    ),
    (
        "ResolvedVolumeSource",
        Shape::Alias(
            "(Extract<VolumeSource, { kind: \"ordinary\" | \"provisioned\" }> & { scope: ScopedVolumeSource }) | (Exclude<VolumeSource, { kind: \"ordinary\" | \"provisioned\" }> & { scope?: null })",
        ),
    ),
    (
        "ResolvedServiceVolume",
        Shape::Object {
            params: "",
            fields: &[
                ("reference", "ServiceVolumeReference"),
                ("source", "ResolvedVolumeSource"),
            ],
        },
    ),
    (
        "ConfigSpec",
        Shape::Object {
            params: "",
            fields: &[("name", "string"), ("content", "number[]?")],
        },
    ),
    (
        "ConfigMount",
        Shape::Object {
            params: "",
            fields: &[
                ("config_name", "string"),
                ("target", "ContainerPath?"),
                ("uid", "number?"),
                ("gid", "number?"),
                ("mode", "number?"),
            ],
        },
    ),
    (
        "ServiceContainerSpec",
        Shape::Object {
            params: "",
            fields: &[
                ("image", "string"),
                ("command", "string[]?"),
                ("entrypoint", "string[]?"),
                ("environment", "{ readonly [key: string]: string }?"),
                ("labels", "ContainerLabels?"),
                ("hostname", "ContainerHostname?"),
                ("extra_hosts", "ExtraHost[]?"),
                ("cap_add", "string[]?"),
                ("cap_drop", "string[]?"),
                ("healthcheck", "HealthcheckSpec?"),
                ("pull_policy", "PullPolicy"),
                ("init", "boolean?"),
                ("user", "string?"),
                ("working_directory", "ContainerPath?"),
                ("tty", "boolean?"),
                ("open_stdin", "boolean?"),
                ("privileged", "boolean?"),
                ("pid_mode", "PidMode?"),
                ("log_driver", "LogDriver?"),
                ("resources", "ContainerResources?"),
                ("stop_timeout_secs", "number?"),
                ("sysctls", "{ readonly [key: string]: string }?"),
                ("restart", "RestartPolicy?"),
                ("config_mounts", "ConfigMount[]?"),
            ],
        },
    ),
    (
        "RequestedServiceSpec",
        Shape::Object {
            params: "",
            fields: &[
                ("name", "ServiceName"),
                ("mode", "ServiceMode"),
                ("container", "ServiceContainerSpec"),
                ("placement", "Placement?"),
                ("ports", "PortPublication[]?"),
                ("volumes", "ServiceVolume[]?"),
                ("mounts", "ServiceMount[]?"),
                ("configs", "ConfigSpec[]?"),
                ("pre_deploy", "PreDeployHook?"),
                ("ingress_proxy_fragment", "IngressProxyFragment?"),
                ("update", "UpdateConfig?"),
            ],
        },
    ),
    (
        "ResolvedServiceSpec",
        Shape::Object {
            params: "",
            fields: &[
                ("service_id", "ServiceId"),
                ("name", "ServiceName"),
                ("mode", "ServiceMode"),
                ("container", "ServiceContainerSpec"),
                ("placement", "Placement?"),
                ("ports", "PortPublication[]?"),
                ("volumes", "ResolvedServiceVolume[]?"),
                ("mounts", "ServiceMount[]?"),
                ("configs", "ConfigSpec[]?"),
                ("pre_deploy", "PreDeployHook?"),
                ("ingress_proxy_fragment", "IngressProxyFragment?"),
                ("update", "ResolvedUpdateConfig?"),
            ],
        },
    ),
    (
        "MembershipObservation",
        Shape::OpenString(MembershipObservation::known_wires()),
    ),
    (
        "HealthObservation",
        Shape::OpenString(HealthObservation::known_wires()),
    ),
    (
        "RpcErrorCode",
        Shape::OpenString(RpcErrorCode::known_wires()),
    ),
    (
        "CertificateAvailability",
        Shape::OpenString(CertificateAvailability::known_wires()),
    ),
    (
        "CertificateFailureKind",
        Shape::OpenString(CertificateFailureKind::known_wires()),
    ),
    (
        "ContainerKind",
        Shape::ClosedString(&["service_container", "pre_deploy_hook"]),
    ),
    (
        "DockerVolumeId",
        Shape::Object {
            params: "",
            fields: &[("machine_id", "MachineId"), ("name", "DockerVolumeName")],
        },
    ),
    (
        "DockerVolumeStorageObservation",
        Shape::InternallyTagged {
            tag: "kind",
            params: "",
            variants: &[
                ("plain", &[("driver", "string")]),
                (
                    "provisioned",
                    &[
                        ("mountpoint", "MachinePath"),
                        ("bound_bytes", "number"),
                        ("used_bytes", "number"),
                    ],
                ),
            ],
        },
    ),
    (
        "DockerVolume",
        Shape::Object {
            params: "",
            fields: &[
                ("id", "DockerVolumeId"),
                ("options", "{ readonly [key: string]: string }"),
                ("labels", "{ readonly [key: string]: string }"),
                ("storage", "DockerVolumeStorageObservation"),
            ],
        },
    ),
    (
        "VolumeObservationFailure",
        Shape::Object {
            params: "",
            fields: &[("id", "DockerVolumeId"), ("error", "RpcError")],
        },
    ),
    (
        "VolumeInventory",
        Shape::Object {
            params: "",
            fields: &[
                ("volumes", "DockerVolume[]"),
                ("failures", "VolumeObservationFailure[]"),
            ],
        },
    ),
    (
        "CreateVolumeReport",
        Shape::InternallyTagged {
            tag: "verification",
            params: "",
            variants: &[
                ("verified", &[("volume", "DockerVolume")]),
                (
                    "unverified",
                    &[("id", "DockerVolumeId"), ("error", "RpcError")],
                ),
            ],
        },
    ),
    (
        "VolumeRemoval",
        Shape::Object {
            params: "",
            fields: &[
                ("id", "DockerVolumeId"),
                ("outcome", "VolumeRemovalOutcome"),
            ],
        },
    ),
    (
        "VolumeRemovalOutcome",
        Shape::InternallyTagged {
            tag: "status",
            params: "",
            variants: &[
                ("removed", &[]),
                ("failed", &[("error", "RpcError")]),
                ("omitted", &[]),
            ],
        },
    ),
    (
        "RemoveVolumesRequest",
        Shape::Object {
            params: "",
            fields: &[("volumes", "DockerVolumeId[]"), ("force", "boolean?")],
        },
    ),
    (
        "DataLoss",
        Shape::InternallyTagged {
            tag: "kind",
            params: "",
            variants: &[("docker_volume", &[("id", "DockerVolumeId")])],
        },
    ),
    (
        "ObservedDataLoss",
        Shape::Object {
            params: "",
            fields: &[("data_loss", "DataLoss[]")],
        },
    ),
    (
        "DataLossConfirmation",
        Shape::Object {
            params: "",
            fields: &[("confirmed", "DataLoss[]")],
        },
    ),
    (
        "UnconfirmedDataLoss",
        Shape::Object {
            params: "",
            fields: &[("missing", "DataLoss[]")],
        },
    ),
    (
        "LocalMachineRemoved",
        Shape::Object {
            params: "",
            fields: &[("reset_warning", "string?")],
        },
    ),
    (
        "ClusterTeardown",
        Shape::Object {
            params: "",
            fields: &[
                ("destroyed_projects", "ProjectName[]"),
                ("machines", "PartialResult<LocalMachineRemoved, RpcError>"),
                ("pairing_revoked", "boolean"),
            ],
        },
    ),
    (
        "ContractDescription",
        Shape::Object {
            params: "",
            fields: &[
                ("machine_id", "MachineId"),
                ("protocol_major", "number"),
                ("daemon_version", "string"),
                ("capabilities", "CapabilityName[]"),
            ],
        },
    ),
    (
        "RpcError",
        Shape::Object {
            params: "",
            fields: &[
                ("code", "RpcErrorCode"),
                ("message", "string"),
                // Per-code JSON (ingest reason, data-loss payload). Not one wire type.
                ("details", "JsonValue?"),
            ],
        },
    ),
    (
        "MachineSuccess",
        Shape::Object {
            params: "<T>",
            fields: &[("machine_id", "MachineId"), ("value", "T")],
        },
    ),
    (
        "MachineFailure",
        Shape::Object {
            params: "<E>",
            fields: &[("machine_id", "MachineId"), ("error", "E")],
        },
    ),
    (
        "PartialResult",
        Shape::Object {
            params: "<T, E>",
            fields: &[
                ("successes", "Array<MachineSuccess<T>>"),
                ("failures", "Array<MachineFailure<E>>"),
                ("omissions", "MachineId[]"),
            ],
        },
    ),
    (
        "ContainerRuntimeObservation",
        Shape::InternallyTagged {
            tag: "state",
            params: "",
            variants: &[
                ("created", &[]),
                ("running", &[("health", "HealthObservation")]),
                ("paused", &[]),
                ("restarting", &[]),
                ("exited", &[("code", "number")]),
                ("removing", &[]),
                ("dead", &[]),
                ("unrecognized", &[("raw", "JsonValue")]),
            ],
        },
    ),
    (
        "PlanOptions",
        Shape::Object {
            params: "",
            fields: &[
                ("force_recreate", "boolean"),
                ("skip_health_monitor", "boolean"),
                ("placement_seed", "number"),
                ("selected", "ServiceAttempt[]"),
            ],
        },
    ),
    (
        "ServiceAttempt",
        Shape::Object {
            params: "",
            fields: &[("name", "ServiceName")],
        },
    ),
    (
        "DeployIntent",
        Shape::Object {
            params: "",
            fields: &[
                ("project_name", "ProjectName"),
                ("target", "RequestedServiceSpec[]"),
                ("options", "PlanOptions"),
            ],
        },
    ),
    (
        "ObservationKind",
        Shape::ClosedString(&["container", "volume"]),
    ),
    (
        "DeployWarning",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                (
                    "observation_failed",
                    &[
                        ("kind", "ObservationKind"),
                        ("machine_id", "MachineId"),
                        ("message", "string"),
                    ],
                ),
                (
                    "observation_omitted",
                    &[("kind", "ObservationKind"), ("machine_id", "MachineId")],
                ),
                (
                    "storage_observation_unknown",
                    &[("machine_id", "MachineId")],
                ),
                ("ingress_hostname", &[("message", "string")]),
                ("observer_relative_hostname_conflict", &[]),
                (
                    "skipped_dependency_health",
                    &[
                        ("dependent", "QualifiedService"),
                        ("dependency", "QualifiedService"),
                    ],
                ),
            ],
        },
    ),
    (
        "PruneRefusal",
        Shape::ClosedString(&[
            "incomplete_snapshot",
            "selected_services",
            "filtered_profiles",
            "guessed_project_name",
        ]),
    ),
    (
        "PreservedVolume",
        Shape::Object {
            params: "",
            fields: &[("id", "DockerVolumeId"), ("machine_name", "MachineName?")],
        },
    ),
    (
        "VolumeToCreate",
        Shape::Object {
            params: "",
            fields: &[
                ("machine_id", "MachineId"),
                ("machine_name", "MachineName?"),
                ("name", "DockerVolumeName"),
                ("maximum_bytes", "ProvisionedVolumeMaximumBytes?"),
            ],
        },
    ),
    (
        "DeployPreview",
        Shape::Object {
            params: "",
            fields: &[
                ("project_name", "ProjectName"),
                ("operations", "OperationRow[]"),
                ("warnings", "DeployWarning[]"),
                ("would_remove", "QualifiedService[]"),
                ("volumes_to_create", "VolumeToCreate[]"),
                ("preserved_volumes", "PreservedVolume[]"),
                ("prune_refusal", "PruneRefusal?"),
            ],
        },
    ),
    (
        "OperationRow",
        Shape::Object {
            params: "",
            fields: &[
                ("index", "number"),
                ("machine_id", "MachineId"),
                ("machine_name", "MachineName?"),
                ("operation", "DeployOperation"),
                ("display_name", "string?"),
                ("service_name", "ServiceName?"),
                ("status", "OperationStatus"),
            ],
        },
    ),
    (
        "OperationStatus",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                ("pending", &[]),
                ("running", &[("phase", "OperationPhase")]),
                ("completed", &[]),
                ("failed", &[("error", "ExecutionError")]),
                ("unexecuted", &[]),
            ],
        },
    ),
    (
        "OperationPhase",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                ("starting", &[]),
                ("creating_container", &[]),
                ("starting_container", &[]),
                (
                    "waiting_for_health",
                    &[
                        ("container_id", "ContainerId"),
                        ("health", "HealthObservation?"),
                        ("elapsed_ms", "number"),
                        ("deadline_ms", "number"),
                    ],
                ),
                (
                    "waiting_for_hook",
                    &[
                        ("container_id", "ContainerId"),
                        ("elapsed_ms", "number"),
                        ("deadline_ms", "number"),
                    ],
                ),
                ("stopping_container", &[]),
                ("removing_container", &[]),
                ("removing_volume", &[]),
                ("compensating", &[]),
            ],
        },
    ),
    (
        "DeployEvent",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                (
                    "progress",
                    &[
                        ("completed", "number"),
                        ("total", "number"),
                        ("rows", "OperationRow[]"),
                    ],
                ),
                ("outcome", &[("outcome", "DeployOutcome")]),
            ],
        },
    ),
    (
        "ReplacementOperation",
        Shape::Object {
            params: "",
            fields: &[
                ("machine_id", "MachineId"),
                ("old_container_id", "ContainerId"),
                ("spec", "ResolvedServiceSpec"),
                ("skip_health_monitor", "boolean"),
            ],
        },
    ),
    (
        "StopContainerPurpose",
        Shape::ClosedString(&["lifecycle", "free_host_ports"]),
    ),
    (
        "DeployOperation",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                (
                    "wait_healthy",
                    &[
                        ("machine_id", "MachineId"),
                        ("dependent", "QualifiedService"),
                        ("dependency", "QualifiedService"),
                    ],
                ),
                (
                    "run_container",
                    &[
                        ("machine_id", "MachineId"),
                        ("spec", "ResolvedServiceSpec"),
                        ("skip_health_monitor", "boolean"),
                    ],
                ),
                (
                    "stop_container",
                    &[
                        ("machine_id", "MachineId"),
                        ("container_id", "ContainerId"),
                        ("purpose", "StopContainerPurpose"),
                    ],
                ),
                (
                    "remove_container",
                    &[("machine_id", "MachineId"), ("container_id", "ContainerId")],
                ),
                (
                    "replace_container",
                    &[
                        ("machine_id", "MachineId"),
                        ("old_container_id", "ContainerId"),
                        ("spec", "ResolvedServiceSpec"),
                        ("skip_health_monitor", "boolean"),
                    ],
                ),
                (
                    "stop_hook",
                    &[("machine_id", "MachineId"), ("container_id", "ContainerId")],
                ),
                (
                    "run_hook",
                    &[
                        ("machine_id", "MachineId"),
                        ("spec", "ResolvedServiceSpec"),
                        ("old_hook_containers", "Array<[MachineId, ContainerId]>"),
                    ],
                ),
                ("remove_volume", &[("id", "DockerVolumeId")]),
            ],
        },
    ),
    (
        "MachineAction",
        Shape::ClosedString(&[
            "CreateContainer",
            "StartContainer",
            "InspectContainer",
            "StopContainer",
            "RemoveContainer",
            "RemoveVolume",
        ]),
    ),
    (
        "HealthFailure",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                ("cancelled", &[]),
                ("timed_out", &[]),
                ("runtime", &[("observation", "ContainerRuntimeObservation")]),
            ],
        },
    ),
    (
        "HookFailure",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                ("cancelled", &[("stop_error", "RpcError | null")]),
                ("timed_out", &[("stop_error", "RpcError | null")]),
                ("exit", &[("code", "number")]),
            ],
        },
    ),
    (
        "DependencyHealthFailure",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                ("cancelled", &[]),
                ("no_containers", &[]),
                ("observation", &[("error", "RpcError")]),
                (
                    "container",
                    &[
                        ("container_id", "ContainerId"),
                        ("failure", "HealthFailure"),
                    ],
                ),
            ],
        },
    ),
    (
        "ExecutionError",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                (
                    "machine",
                    &[("action", "MachineAction"), ("error", "RpcError")],
                ),
                (
                    "health",
                    &[
                        ("container_id", "ContainerId"),
                        ("failure", "HealthFailure"),
                    ],
                ),
                (
                    "dependency_health",
                    &[
                        ("dependency", "QualifiedService"),
                        ("failure", "DependencyHealthFailure"),
                    ],
                ),
                (
                    "hook",
                    &[("container_id", "ContainerId"), ("failure", "HookFailure")],
                ),
                ("cancelled", &[]),
            ],
        },
    ),
    (
        "StopAttempt",
        Shape::InternallyTagged {
            tag: "type",
            params: "<E = ExecutionError>",
            variants: &[("stopped", &[]), ("failed", &[("error", "E")])],
        },
    ),
    (
        "RestartAttempt",
        Shape::InternallyTagged {
            tag: "type",
            params: "<E = ExecutionError>",
            variants: &[
                ("not_attempted", &[]),
                ("restarted", &[]),
                ("failed", &[("error", "E")]),
            ],
        },
    ),
    (
        "ReplacementCompensation",
        Shape::InternallyTagged {
            tag: "type",
            params: "<E = ExecutionError>",
            variants: &[
                ("start_first", &[("stop_new_container", "StopAttempt<E>")]),
                (
                    "stop_first",
                    &[
                        ("stop_new_container", "StopAttempt<E>"),
                        ("restart_old_container", "RestartAttempt<E>"),
                    ],
                ),
            ],
        },
    ),
    (
        "FailedOperation",
        Shape::InternallyTagged {
            tag: "type",
            params: "<E = ExecutionError>",
            variants: &[
                (
                    "operation",
                    &[("operation", "DeployOperation"), ("error", "E")],
                ),
                (
                    "replacement_health",
                    &[
                        ("operation", "ReplacementOperation"),
                        ("error", "E"),
                        ("compensation", "ReplacementCompensation<E>"),
                    ],
                ),
            ],
        },
    ),
    (
        "DeployOutcome",
        Shape::InternallyTagged {
            tag: "type",
            params: "<E = ExecutionError>",
            variants: &[
                ("success", &[("completed", "DeployOperation[]")]),
                (
                    "failed",
                    &[
                        ("completed", "DeployOperation[]"),
                        ("failed", "FailedOperation<E>"),
                        ("unexecuted", "DeployOperation[]"),
                    ],
                ),
            ],
        },
    ),
    (
        "MachineRuntime",
        Shape::Object {
            params: "",
            fields: &[
                ("daemon_version", "string"),
                ("docker_version", "string"),
                ("hostname", "string"),
                ("architecture", "string"),
                ("os_pretty_name", "string"),
                ("kernel_version", "string"),
            ],
        },
    ),
    (
        "Machine",
        Shape::Object {
            params: "",
            fields: &[
                ("id", "MachineId"),
                ("name", "MachineName"),
                ("subnet", "MachineSubnet"),
                ("public_key", "WireGuardPublicKey"),
                ("public_ip", "string?"),
                ("advertised_endpoints", "AdvertisedEndpoint[]"),
                ("runtime", "MachineRuntime"),
            ],
        },
    ),
    (
        "RegisterRequest",
        Shape::Object {
            params: "",
            fields: &[
                ("name", "MachineName"),
                ("storage", "StorageChoice"),
                ("public_key", "WireGuardPublicKey"),
                ("public_ip", "string?"),
                ("advertised_endpoints", "AdvertisedEndpoint[]"),
                ("runtime", "MachineRuntime"),
            ],
        },
    ),
    (
        "Registered",
        Shape::Object {
            params: "",
            fields: &[
                ("assigned_machine", "Machine"),
                ("visible_peers", "Machine[]"),
                ("target_versions", "{ readonly [key: string]: number }"),
            ],
        },
    ),
    (
        "RttStatistics",
        Shape::Object {
            params: "",
            fields: &[("median_ns", "number"), ("population_stddev_ns", "number")],
        },
    ),
    (
        "GlobalReconcileFailureObservation",
        Shape::Object {
            params: "",
            fields: &[
                ("service", "QualifiedService"),
                ("last_error", "string"),
                ("observed_at", "string"),
            ],
        },
    ),
    (
        "MachineObservation",
        Shape::Object {
            params: "",
            fields: &[
                ("machine", "Machine"),
                ("membership", "MembershipObservation"),
                ("storage", "MachineStorageObservation?"),
                ("selected_endpoint", "SelectedEndpoint | null"),
                ("rtt", "RttStatistics?"),
                (
                    "global_reconcile_failures",
                    "GlobalReconcileFailureObservation[]?",
                ),
            ],
        },
    ),
    (
        "ContainerObservation",
        Shape::Object {
            params: "",
            fields: &[
                ("container_id", "ContainerId"),
                ("display_name", "string"),
                ("created_at_unix_nanos", "number"),
                ("machine_id", "MachineId"),
                ("project_name", "ProjectName"),
                ("kind", "ContainerKind"),
                ("runtime", "ContainerRuntimeObservation"),
                ("effective_healthcheck", "HealthcheckSpec | null"),
                ("resolved_spec", "ResolvedServiceSpec"),
                ("address", "ContainerAddress | null"),
                ("labels", "{ readonly [key: string]: string }"),
            ],
        },
    ),
    ("ServiceContainer", Shape::Alias("ContainerObservation")),
    ("HookContainer", Shape::Alias("ContainerObservation")),
    (
        "ServiceObservation",
        Shape::Object {
            params: "",
            fields: &[
                ("identity", "QualifiedService"),
                ("service_id", "ServiceId"),
                ("containers", "ServiceContainer[]"),
                ("hook_containers", "HookContainer[]"),
            ],
        },
    ),
    (
        "CertificateBackoff",
        Shape::Object {
            params: "",
            fields: &[
                ("failure_kind", "CertificateFailureKind"),
                ("next_attempt_at", "string"),
                ("failures", "number"),
            ],
        },
    ),
    (
        "CertificateObservation",
        Shape::Object {
            params: "",
            fields: &[
                ("hostname", "IngressHost"),
                ("status", "CertificateAvailability"),
                ("last_error", "string?"),
                ("backoff", "CertificateBackoff?"),
            ],
        },
    ),
    (
        "RuntimeWatchIncompleteIds",
        Shape::Object {
            params: "",
            fields: &[
                ("machines", "MachineId[]"),
                ("containers", "ContainerId[]"),
                ("volumes", "DockerVolumeId[]"),
                ("certificates", "IngressHost[]"),
            ],
        },
    ),
    (
        "RuntimeWatchFrame",
        Shape::Object {
            params: "",
            fields: &[
                ("machines", "MachineObservation[]"),
                ("containers", "ContainerObservation[]"),
                ("volumes", "DockerVolume[]"),
                ("certificates", "CertificateObservation[]"),
                ("hosted_dns_hostname", "string?"),
                ("incomplete_ids", "RuntimeWatchIncompleteIds"),
                ("observed_at", "string"),
            ],
        },
    ),
];
