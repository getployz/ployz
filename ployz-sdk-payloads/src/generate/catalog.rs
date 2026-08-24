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
    Additive {
        params: &'static str,
        fields: &'static [(&'static str, &'static str)],
    },
    ExternallyTagged {
        params: &'static str,
        variants: &'static [(&'static str, Option<&'static str>)],
    },
    InternallyTagged {
        tag: &'static str,
        params: &'static str,
        variants: &'static [(&'static str, &'static [(&'static str, &'static str)])],
    },
}

pub(super) const PAYLOADS: &[(&str, Shape)] = &[
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
        Shape::Additive {
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
                (
                    "named",
                    &[
                        ("name", "DockerVolumeName"),
                        ("external", "boolean?"),
                        ("driver", "VolumeDriver?"),
                        ("labels", "{ readonly [key: string]: string }?"),
                        ("no_copy", "boolean?"),
                        ("subpath", "string?"),
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
        Shape::Additive {
            params: "",
            fields: &[
                ("name", "string"),
                ("options", "{ readonly [key: string]: string }"),
            ],
        },
    ),
    (
        "DeviceMapping",
        Shape::Additive {
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
        Shape::Additive {
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
        Shape::Additive {
            params: "",
            fields: &[("soft", "number"), ("hard", "number")],
        },
    ),
    (
        "ContainerResources",
        Shape::Additive {
            params: "",
            fields: &[
                ("cpu_nanos", "number?"),
                ("memory_bytes", "number?"),
                ("memory_reservation_bytes", "number?"),
                ("shared_memory_bytes", "number?"),
                ("devices", "DeviceMapping[]?"),
                ("device_reservations", "DeviceReservation[]?"),
                ("ulimits", "{ readonly [key: string]: Ulimit }?"),
            ],
        },
    ),
    (
        "UpdateConfig",
        Shape::Additive {
            params: "",
            fields: &[("order", "UpdateOrder?"), ("monitor_millis", "number?")],
        },
    ),
    (
        "ResolvedUpdateConfig",
        Shape::Additive {
            params: "",
            fields: &[("order", "UpdateOrder"), ("monitor_millis", "number?")],
        },
    ),
    (
        "Placement",
        Shape::Additive {
            params: "",
            fields: &[("machines", "MachineTarget[]?")],
        },
    ),
    (
        "PreDeployHook",
        Shape::Additive {
            params: "",
            fields: &[
                ("command", "string[]"),
                ("environment", "{ readonly [key: string]: string }?"),
                ("privileged", "boolean?"),
                ("timeout_millis", "number?"),
                ("user", "string?"),
            ],
        },
    ),
    (
        "ServiceMount",
        Shape::Additive {
            params: "",
            fields: &[
                ("volume", "ServiceVolumeReference"),
                ("target", "ContainerPath"),
                ("read_only", "boolean?"),
            ],
        },
    ),
    (
        "ServiceVolume",
        Shape::Additive {
            params: "",
            fields: &[
                ("reference", "ServiceVolumeReference"),
                ("source", "VolumeSource"),
            ],
        },
    ),
    (
        "ConfigSpec",
        Shape::Additive {
            params: "",
            fields: &[("name", "string"), ("content", "number[]?")],
        },
    ),
    (
        "ConfigMount",
        Shape::Additive {
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
        Shape::Additive {
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
        Shape::Additive {
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
                ("caddy_config", "string?"),
                ("update", "UpdateConfig?"),
            ],
        },
    ),
    (
        "ResolvedServiceSpec",
        Shape::Additive {
            params: "",
            fields: &[
                ("service_id", "ServiceId"),
                ("name", "ServiceName"),
                ("mode", "ServiceMode"),
                ("container", "ServiceContainerSpec"),
                ("placement", "Placement?"),
                ("ports", "PortPublication[]?"),
                ("volumes", "ServiceVolume[]?"),
                ("mounts", "ServiceMount[]?"),
                ("configs", "ConfigSpec[]?"),
                ("pre_deploy", "PreDeployHook?"),
                ("caddy_config", "string?"),
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
        Shape::Additive {
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
        Shape::Additive {
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
        Shape::Additive {
            params: "",
            fields: &[("id", "DockerVolumeId"), ("error", "RpcError")],
        },
    ),
    (
        "VolumeInventory",
        Shape::Additive {
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
        "RemoveVolumesRequest",
        Shape::Additive {
            params: "",
            fields: &[("volumes", "DockerVolumeId[]"), ("force", "boolean?")],
        },
    ),
    (
        "DataLoss",
        Shape::ExternallyTagged {
            params: "",
            variants: &[("DockerVolume", Some("DockerVolumeId"))],
        },
    ),
    (
        "ObservedDataLoss",
        Shape::Additive {
            params: "",
            fields: &[("data_loss", "DataLoss[]")],
        },
    ),
    (
        "UnconfirmedDataLoss",
        Shape::Additive {
            params: "",
            fields: &[("missing", "DataLoss[]")],
        },
    ),
    (
        "LocalMachineRemoved",
        Shape::Additive {
            params: "",
            fields: &[("reset_warning", "string?")],
        },
    ),
    (
        "ClusterTeardown",
        Shape::Additive {
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
        Shape::Additive {
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
        Shape::Additive {
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
        Shape::Additive {
            params: "<T>",
            fields: &[("machine_id", "MachineId"), ("value", "T")],
        },
    ),
    (
        "MachineFailure",
        Shape::Additive {
            params: "<E>",
            fields: &[("machine_id", "MachineId"), ("error", "E")],
        },
    ),
    (
        "PartialResult",
        Shape::Additive {
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
            ],
        },
    ),
    (
        "PlanOptions",
        Shape::Additive {
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
        Shape::Additive {
            params: "",
            fields: &[("name", "ServiceName")],
        },
    ),
    (
        "ProvisionedVolume",
        Shape::Additive {
            params: "",
            fields: &[
                ("service", "ServiceName"),
                ("reference", "ServiceVolumeReference"),
                ("maximum_bytes", "ProvisionedVolumeMaximumBytes"),
            ],
        },
    ),
    (
        "DeployIntent",
        Shape::Additive {
            params: "",
            fields: &[
                ("project_name", "ProjectName"),
                ("target", "RequestedServiceSpec[]"),
                ("provisioned_volumes", "ProvisionedVolume[]"),
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
        Shape::ExternallyTagged {
            params: "",
            variants: &[
                (
                    "ObservationFailed",
                    Some("{ kind: ObservationKind; machine_id: MachineId; message: string }"),
                ),
                (
                    "ObservationOmitted",
                    Some("{ kind: ObservationKind; machine_id: MachineId }"),
                ),
                ("IngressHostname", Some("string")),
                ("ObserverRelativeHostnameConflict", None),
                (
                    "SkippedDependencyHealth",
                    Some("{ dependent: QualifiedService; dependency: QualifiedService }"),
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
        Shape::Additive {
            params: "",
            fields: &[("id", "DockerVolumeId"), ("machine_name", "MachineName?")],
        },
    ),
    (
        "DeployPreview",
        Shape::Additive {
            params: "",
            fields: &[
                ("project_name", "ProjectName"),
                ("operations", "OperationRow[]"),
                ("warnings", "DeployWarning[]"),
                ("would_remove", "QualifiedService[]"),
                ("preserved_volumes", "PreservedVolume[]"),
                ("prune_refusal", "PruneRefusal?"),
            ],
        },
    ),
    (
        "OperationRow",
        Shape::Additive {
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
                ("creating_volume", &[]),
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
        Shape::Additive {
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
        "DeployOperation",
        Shape::InternallyTagged {
            tag: "type",
            params: "",
            variants: &[
                (
                    "create_volume",
                    &[("machine_id", "MachineId"), ("volume", "ServiceVolume")],
                ),
                (
                    "create_provisioned_volume",
                    &[
                        ("machine_id", "MachineId"),
                        ("volume", "ServiceVolume"),
                        ("maximum_bytes", "ProvisionedVolumeMaximumBytes"),
                    ],
                ),
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
                    &[("machine_id", "MachineId"), ("container_id", "ContainerId")],
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
            "CreateVolume",
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
        "RestartAttempt",
        Shape::ExternallyTagged {
            params: "<E = ExecutionError>",
            variants: &[
                ("NotAttempted", None),
                ("Attempted", Some("SerdeResult<null, E>")),
            ],
        },
    ),
    (
        "ReplacementCompensation",
        Shape::ExternallyTagged {
            params: "<E = ExecutionError>",
            variants: &[
                (
                    "StartFirst",
                    Some("{ stop_new_container: SerdeResult<null, E> }"),
                ),
                (
                    "StopFirst",
                    Some(
                        "{ stop_new_container: SerdeResult<null, E>; restart_old_container: RestartAttempt<E> }",
                    ),
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
        Shape::Additive {
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
        Shape::Additive {
            params: "",
            fields: &[
                ("id", "MachineId"),
                ("name", "MachineName"),
                ("subnet", "MachineSubnet"),
                ("management_address", "ManagementAddress"),
                ("public_key", "WireGuardPublicKey"),
                ("public_ip", "string?"),
                ("advertised_endpoints", "AdvertisedEndpoint[]"),
                ("runtime", "MachineRuntime"),
            ],
        },
    ),
    (
        "RegisterRequest",
        Shape::Additive {
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
        Shape::Additive {
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
        Shape::Additive {
            params: "",
            fields: &[("median_ns", "number"), ("population_stddev_ns", "number")],
        },
    ),
    (
        "MachineObservation",
        Shape::Additive {
            params: "",
            fields: &[
                ("machine", "Machine"),
                ("membership", "MembershipObservation"),
                ("storage", "MachineStorageObservation?"),
                ("selected_endpoint", "SelectedEndpoint | null"),
                ("rtt", "RttStatistics?"),
            ],
        },
    ),
    (
        "ContainerObservation",
        Shape::Additive {
            params: "",
            fields: &[
                ("container_id", "ContainerId"),
                ("display_name", "string"),
                ("created_at_unix_nanos", "number"),
                ("machine_id", "MachineId"),
                ("project_name", "ProjectName"),
                ("service_id", "ServiceId"),
                ("service_name", "ServiceName"),
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
        Shape::Additive {
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
        Shape::Additive {
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
        Shape::Additive {
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
        Shape::Additive {
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
        Shape::Additive {
            params: "",
            fields: &[
                ("machines", "MachineObservation[]"),
                ("containers", "ContainerObservation[]"),
                ("services", "ServiceObservation[]"),
                ("volumes", "DockerVolume[]"),
                ("certificates", "CertificateObservation[]"),
                ("hosted_dns_hostname", "string?"),
                ("incomplete_ids", "RuntimeWatchIncompleteIds"),
                ("observed_at", "string"),
            ],
        },
    ),
];
