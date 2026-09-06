//! Shape catalog for generated `@ployz/sdk` TypeScript.

use super::evidence::{RustEvidence, TaggedEvidence};
use crate::values::{
    container_id, data_loss, deploy_event_progress, deploy_operations, deploy_outcome,
    deploy_outcome_failed, deploy_warnings, docker_volume, execution_error_machine, host_port,
    ingress_host, ingress_port, named_volume_with_driver, operation_phases, operation_statuses,
    provisioned_volume_source, replacement_operation, rpc_error, service_volume, to_value,
};
use ployz_core::{
    BindPropagation, BindRecursive, CertificateAvailability, CertificateFailureKind,
    ConfiguredHealthcheck, ContainerKind, ContainerRuntimeObservation, CreateVolumeReport,
    DataLoss, DependencyHealthFailure, DeployEvent, DeployOperation, DeployOutcome, DeployWarning,
    DockerVolumeName, DockerVolumeStorageObservation, ExecutionError, FailedOperation,
    HealthFailure, HealthObservation, HealthcheckCommand, HealthcheckSpec, HookFailure, HostBind,
    HttpProtocol, IngressHostname, MachineAction, MachinePath, MachineStorageObservation,
    MembershipObservation, ObservationKind, OperationPhase, OperationStatus, PortPublication,
    PruneRefusal, PullPolicy, QualifiedService, ReplacementCompensation, RestartAttempt,
    RestartPolicy, RpcErrorCode, ServiceMode, StopAttempt, StopContainerPurpose, StorageChoice,
    TransportProtocol, UpdateOrder, VolumeRemovalOutcome, VolumeSource,
};
use serde_json::{Value, json};
use std::{
    net::IpAddr,
    num::{NonZeroU32, NonZeroU64},
};

pub(super) enum Shape {
    Alias(&'static str),
    Branded,
    OpenString(&'static [&'static str]),
    ClosedString {
        known: &'static [&'static str],
        examples: fn() -> Vec<Value>,
    },
    Object {
        params: &'static str,
        fields: &'static [(&'static str, &'static str)],
    },
    InternallyTagged {
        evidence: &'static dyn TaggedEvidence,
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
        Shape::ClosedString {
            known: &["always", "missing", "never"],
            examples: || {
                vec![
                    to_value(&PullPolicy::Always),
                    to_value(&PullPolicy::Missing),
                    to_value(&PullPolicy::Never),
                ]
            },
        },
    ),
    (
        "StorageChoice",
        Shape::ClosedString {
            known: &["none", "zfs"],
            examples: || {
                vec![
                    to_value(&StorageChoice::None),
                    to_value(&StorageChoice::Zfs),
                ]
            },
        },
    ),
    (
        "MachineStorageObservation",
        Shape::InternallyTagged {
            evidence: &RustEvidence::<MachineStorageObservation>(|| {
                vec![
                    MachineStorageObservation::Stateless,
                    MachineStorageObservation::Ready,
                    MachineStorageObservation::Pool {
                        size_bytes: NonZeroU64::new(4_294_967_296)
                            .expect("fixture capacity is nonzero"),
                        used_bytes: 3_865_470_566,
                        free_bytes: 429_496_730,
                    },
                ]
            }),
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
        Shape::ClosedString {
            known: &["start_first", "stop_first"],
            examples: || {
                vec![
                    to_value(&UpdateOrder::StartFirst),
                    to_value(&UpdateOrder::StopFirst),
                ]
            },
        },
    ),
    (
        "HttpProtocol",
        Shape::ClosedString {
            known: &["http", "https"],
            examples: || {
                vec![
                    to_value(&HttpProtocol::Http),
                    to_value(&HttpProtocol::Https),
                ]
            },
        },
    ),
    (
        "TransportProtocol",
        Shape::ClosedString {
            known: &["tcp", "udp"],
            examples: || {
                vec![
                    to_value(&TransportProtocol::Tcp),
                    to_value(&TransportProtocol::Udp),
                ]
            },
        },
    ),
    (
        "ServiceMode",
        Shape::InternallyTagged {
            evidence: &RustEvidence::<ServiceMode>(|| {
                vec![
                    ServiceMode::Replicated {
                        replicas: NonZeroU32::MIN,
                    },
                    ServiceMode::Global,
                ]
            }),
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
            evidence: &RustEvidence::<IngressHostname>(|| {
                vec![
                    IngressHostname::cluster_domain(),
                    IngressHostname::cluster_domain_label("api")
                        .expect("fixture Cluster Domain label is valid"),
                    IngressHostname::Explicit {
                        hostname: ingress_host("app.example.com"),
                    },
                ]
            }),
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
            evidence: &RustEvidence::<HostBind>(|| {
                vec![
                    HostBind::All,
                    HostBind::Address {
                        address: IpAddr::from([127, 0, 0, 1]),
                    },
                    serde_json::from_value::<HostBind>(
                        json!({ "kind": "prefix", "prefix": "10.0.0.0/8" }),
                    )
                    .expect("fixture HostBind prefix is valid"),
                ]
            }),
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
            evidence: &RustEvidence::<PortPublication>(|| vec![ingress_port(), host_port()]),
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
            evidence: &RustEvidence::<VolumeSource>(|| {
                vec![
                    ployz_core::RawVolumeSource::Bind {
                        machine_path: MachinePath::parse("/data")
                            .expect("fixture bind path is valid"),
                        create_machine_path: false,
                        propagation: Some(BindPropagation::Private),
                        recursive: Some(BindRecursive::Disabled),
                    }
                    .admit()
                    .expect("valid volume declaration"),
                    ployz_core::RawVolumeSource::External {
                        name: DockerVolumeName::parse("shared")
                            .expect("fixture external Volume name is valid"),
                    }
                    .admit()
                    .expect("valid volume declaration"),
                    service_volume().source,
                    named_volume_with_driver().source,
                    provisioned_volume_source(),
                    ployz_core::RawVolumeSource::Tmpfs {
                        size_bytes: Some(64),
                        mode: Some(0o755),
                        options: Vec::new(),
                    }
                    .admit()
                    .expect("valid volume declaration"),
                ]
            }),
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
            evidence: &RustEvidence::<HealthcheckSpec>(|| {
                vec![
                    HealthcheckSpec::Disabled,
                    HealthcheckSpec::Configured(ConfiguredHealthcheck {
                        test: HealthcheckCommand::parse(["CMD", "true"])
                            .expect("fixture healthcheck command is valid"),
                        interval_millis: None,
                        timeout_millis: None,
                        start_period_millis: None,
                        start_interval_millis: None,
                        retries: None,
                    }),
                ]
            }),
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
            evidence: &RustEvidence::<RestartPolicy>(|| {
                vec![
                    RestartPolicy::No,
                    RestartPolicy::Always,
                    RestartPolicy::UnlessStopped,
                    RestartPolicy::OnFailure {
                        maximum_retry_count: Some(2),
                    },
                ]
            }),
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
        Shape::ClosedString {
            known: &["service_container", "pre_deploy_hook"],
            examples: || {
                vec![
                    to_value(&ContainerKind::ServiceContainer),
                    to_value(&ContainerKind::PreDeployHook),
                ]
            },
        },
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
            evidence: &RustEvidence::<DockerVolumeStorageObservation>(|| {
                vec![
                    DockerVolumeStorageObservation::Plain {
                        driver: "local".into(),
                    },
                    DockerVolumeStorageObservation::Provisioned {
                        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/data")
                            .expect("fixture mountpoint is valid"),
                        bound_bytes: NonZeroU64::new(1_073_741_824)
                            .expect("fixture Provisioned Volume bound is positive"),
                        used_bytes: 966_367_642,
                    },
                ]
            }),
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
            evidence: &RustEvidence::<CreateVolumeReport>(|| {
                vec![
                    CreateVolumeReport::Verified {
                        volume: docker_volume(),
                    },
                    CreateVolumeReport::Unverified {
                        id: docker_volume().id,
                        error: rpc_error(),
                    },
                ]
            }),
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
            evidence: &RustEvidence::<VolumeRemovalOutcome>(|| {
                vec![
                    VolumeRemovalOutcome::Removed,
                    VolumeRemovalOutcome::Failed { error: rpc_error() },
                    VolumeRemovalOutcome::Omitted,
                ]
            }),
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
            evidence: &RustEvidence::<DataLoss>(|| vec![data_loss()]),
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
            evidence: &RustEvidence::<ContainerRuntimeObservation>(|| {
                vec![
                    ContainerRuntimeObservation::Created,
                    ContainerRuntimeObservation::Running {
                        health: HealthObservation::Healthy,
                    },
                    ContainerRuntimeObservation::Paused,
                    ContainerRuntimeObservation::Restarting,
                    ContainerRuntimeObservation::Exited { code: 0 },
                    ContainerRuntimeObservation::Removing,
                    ContainerRuntimeObservation::Dead,
                    ContainerRuntimeObservation::Unknown {
                        raw: json!({ "Status": "hibernating", "ExitCode": 0 }),
                    },
                ]
            }),
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
        Shape::ClosedString {
            known: &["container", "volume"],
            examples: || {
                vec![
                    to_value(&ObservationKind::Container),
                    to_value(&ObservationKind::Volume),
                ]
            },
        },
    ),
    (
        "DeployWarning",
        Shape::InternallyTagged {
            evidence: &RustEvidence::<DeployWarning>(|| deploy_warnings().into()),
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
        Shape::ClosedString {
            known: &[
                "incomplete_snapshot",
                "selected_services",
                "filtered_profiles",
                "guessed_project_name",
            ],
            examples: || {
                vec![
                    to_value(&PruneRefusal::IncompleteSnapshot),
                    to_value(&PruneRefusal::SelectedServices),
                    to_value(&PruneRefusal::FilteredProfiles),
                    to_value(&PruneRefusal::GuessedProjectName),
                ]
            },
        },
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
            evidence: &RustEvidence::<OperationStatus>(|| operation_statuses().into()),
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
            evidence: &RustEvidence::<OperationPhase>(|| operation_phases().into()),
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
            evidence: &RustEvidence::<DeployEvent>(|| {
                vec![
                    deploy_event_progress(),
                    DeployEvent::Outcome {
                        outcome: deploy_outcome(),
                    },
                ]
            }),
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
        Shape::ClosedString {
            known: &["lifecycle", "free_host_ports"],
            examples: || {
                vec![
                    to_value(&StopContainerPurpose::Lifecycle),
                    to_value(&StopContainerPurpose::FreeHostPorts),
                ]
            },
        },
    ),
    (
        "DeployOperation",
        Shape::InternallyTagged {
            evidence: &RustEvidence::<DeployOperation>(|| deploy_operations().into()),
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
        Shape::ClosedString {
            known: &[
                "CreateContainer",
                "StartContainer",
                "InspectContainer",
                "StopContainer",
                "RemoveContainer",
                "RemoveVolume",
            ],
            examples: || {
                vec![
                    to_value(&MachineAction::CreateContainer),
                    to_value(&MachineAction::StartContainer),
                    to_value(&MachineAction::InspectContainer),
                    to_value(&MachineAction::StopContainer),
                    to_value(&MachineAction::RemoveContainer),
                    to_value(&MachineAction::RemoveVolume),
                ]
            },
        },
    ),
    (
        "HealthFailure",
        Shape::InternallyTagged {
            evidence: &RustEvidence::<HealthFailure>(|| {
                vec![
                    HealthFailure::Cancelled,
                    HealthFailure::TimedOut,
                    HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Restarting,
                    },
                ]
            }),
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
            evidence: &RustEvidence::<HookFailure>(|| {
                vec![
                    HookFailure::Cancelled { stop_error: None },
                    HookFailure::TimedOut {
                        stop_error: Some(rpc_error()),
                    },
                    HookFailure::Exit { code: 7 },
                ]
            }),
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
            evidence: &RustEvidence::<DependencyHealthFailure>(|| {
                vec![
                    DependencyHealthFailure::Cancelled,
                    DependencyHealthFailure::NoContainers,
                    DependencyHealthFailure::Observation { error: rpc_error() },
                    DependencyHealthFailure::Container {
                        container_id: container_id(),
                        failure: HealthFailure::TimedOut,
                    },
                ]
            }),
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
            evidence: &RustEvidence::<ExecutionError>(|| {
                vec![
                    execution_error_machine(),
                    ExecutionError::Health {
                        container_id: container_id(),
                        failure: HealthFailure::TimedOut,
                    },
                    ExecutionError::DependencyHealth {
                        dependency: QualifiedService::parse("app/db")
                            .expect("fixture has a valid qualified Service name"),
                        failure: DependencyHealthFailure::NoContainers,
                    },
                    ExecutionError::Hook {
                        container_id: container_id(),
                        failure: HookFailure::Exit { code: 1 },
                    },
                    ExecutionError::Cancelled,
                ]
            }),
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
            evidence: &RustEvidence::<StopAttempt<ExecutionError>>(|| {
                vec![
                    StopAttempt::<ExecutionError>::Stopped,
                    StopAttempt::Failed {
                        error: ExecutionError::Cancelled,
                    },
                ]
            }),
            tag: "type",
            params: "<E = ExecutionError>",
            variants: &[("stopped", &[]), ("failed", &[("error", "E")])],
        },
    ),
    (
        "RestartAttempt",
        Shape::InternallyTagged {
            evidence: &RustEvidence::<RestartAttempt<ExecutionError>>(|| {
                vec![
                    RestartAttempt::<ExecutionError>::NotAttempted,
                    RestartAttempt::<ExecutionError>::Restarted,
                    RestartAttempt::Failed {
                        error: ExecutionError::Cancelled,
                    },
                ]
            }),
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
            evidence: &RustEvidence::<ReplacementCompensation<ExecutionError>>(|| {
                vec![
                    ReplacementCompensation::<ExecutionError>::StartFirst {
                        stop_new_container: StopAttempt::Stopped,
                    },
                    ReplacementCompensation::<ExecutionError>::StopFirst {
                        stop_new_container: StopAttempt::Stopped,
                        restart_old_container: RestartAttempt::NotAttempted,
                    },
                ]
            }),
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
            evidence: &RustEvidence::<FailedOperation<ExecutionError>>(|| {
                let DeployOutcome::Failed { failed, .. } = deploy_outcome_failed() else {
                    panic!("failed fixture is Failed");
                };
                vec![
                    failed,
                    FailedOperation::ReplacementHealth {
                        operation: replacement_operation(),
                        error: execution_error_machine(),
                        compensation: ReplacementCompensation::<ExecutionError>::StartFirst {
                            stop_new_container: StopAttempt::Stopped,
                        },
                    },
                ]
            }),
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
            evidence: &RustEvidence::<DeployOutcome<ExecutionError>>(|| {
                vec![deploy_outcome(), deploy_outcome_failed()]
            }),
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
