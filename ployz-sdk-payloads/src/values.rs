//! Rust values that become JSON fixtures and Additive serde examples.

use std::{
    collections::BTreeMap,
    net::{IpAddr, Ipv6Addr, SocketAddr},
    num::{NonZeroU16, NonZeroU32},
};

use ployz_core::{
    AdvertisedEndpoint, BindPropagation, BindRecursive, CapabilityName, CertificateAvailability,
    CertificateBackoff, CertificateFailureKind, CertificateObservation, ConfiguredHealthcheck,
    ContainerId, ContainerKind, ContainerObservation, ContainerPath, ContainerResources,
    ContainerRuntimeObservation, ContractDescription, DESCRIBE_CONTRACT_CAPABILITY, DataLoss,
    DeployEvent, DeployIntent, DeployOperation, DeployOutcome, DeployPreview, DeployWarning,
    DockerVolume, DockerVolumeId, DockerVolumeName, ExecutionError, FailedOperation, HealthFailure,
    HealthObservation, HealthcheckCommand, HealthcheckSpec, HookContainer, HookFailure, HostBind,
    HttpProtocol, IngressHost, IngressHostname, LocalMachineRemoved, LogDriver, Machine,
    MachineAction, MachineFailure, MachineId, MachineName, MachineObservation, MachinePath,
    MachineRuntime, MachineSuccess, ManagementAddress, MembershipObservation, ObservationKind,
    ObservedDataLoss, OperationPhase, OperationRow, OperationStatus, PROTOCOL_MAJOR, PartialResult,
    Placement, PlanOptions, PortPublication, PreDeployHook, PreservedVolume, ProjectName,
    PruneRefusal, PullPolicy, RemoveVolumesRequest, ReplacementCompensation, ReplacementOperation,
    RequestedServiceSpec, ResolvedServiceSpec, ResolvedUpdateConfig, RestartAttempt, RestartPolicy,
    RpcError, RpcErrorCode, RttStatistics, RuntimeWatchFrame, RuntimeWatchIncompleteIds,
    SelectedEndpoint, ServiceAttempt, ServiceContainer, ServiceId, ServiceMode, ServiceMount,
    ServiceName, ServiceObservation, ServiceVolume, ServiceVolumeReference, TransportProtocol,
    UnconfirmedDataLoss, UpdateConfig, UpdateOrder, VolumeSource, WireGuardPublicKey,
};
use serde_json::{Value, json};

const MACHINE_ID_HEX: &str = "0123456789abcdef0123456789abcdef";
const OTHER_MACHINE_ID_HEX: &str = "fedcba9876543210fedcba9876543210";
const SERVICE_ID_HEX: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTAINER_ID_HEX: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const INCOMPLETE_CONTAINER_ID_HEX: &str =
    "2222222222222222222222222222222222222222222222222222222222222222";

/// JSON fixtures keyed by stable names, produced from Rust values.
#[must_use]
pub fn fixtures() -> BTreeMap<String, Value> {
    let mut fixtures = BTreeMap::new();
    fixtures.insert(
        "contract_description".into(),
        to_value(&contract_description()),
    );
    fixtures.insert(
        "contract_description_unknown_fields".into(),
        with_unknown_field(
            to_value(&contract_description()),
            "build_revision",
            json!("future"),
        ),
    );
    fixtures.insert("rpc_error".into(), to_value(&rpc_error()));
    fixtures.insert("docker_volume".into(), to_value(&docker_volume()));
    fixtures.insert("data_loss".into(), to_value(&data_loss()));
    fixtures.insert("observed_data_loss".into(), to_value(&observed_data_loss()));
    fixtures.insert(
        "observed_data_loss_empty".into(),
        to_value(&ObservedDataLoss {
            data_loss: Vec::new(),
        }),
    );
    fixtures.insert(
        "unconfirmed_data_loss".into(),
        to_value(&unconfirmed_data_loss()),
    );
    fixtures.insert(
        "local_machine_removed".into(),
        to_value(&LocalMachineRemoved::default()),
    );
    fixtures.insert(
        "local_machine_removed_reset_warning".into(),
        to_value(&LocalMachineRemoved {
            reset_warning: Some("replicated delete failed".into()),
        }),
    );
    fixtures.insert(
        "docker_volume_unknown_fields".into(),
        with_unknown_field(to_value(&docker_volume()), "quota_bytes", json!(1)),
    );
    fixtures.insert(
        "remove_volumes_request".into(),
        to_value(&remove_volumes_request()),
    );
    fixtures.insert("partial_result".into(), to_value(&partial_result()));
    fixtures.insert(
        "membership_observation_unknown".into(),
        json!("future_membership"),
    );
    fixtures.insert("health_observation_unknown".into(), json!("degraded"));
    fixtures.insert("certificate_availability_unknown".into(), json!("renewing"));
    fixtures.insert(
        "certificate_failure_kind_unknown".into(),
        json!("rate_limited"),
    );
    fixtures.insert(
        "container_runtime_unknown".into(),
        json!({
            "state": "hibernating",
            "wake_at": "tomorrow",
            "vendor": { "reason": 7 }
        }),
    );
    fixtures.insert(
        "container_runtime_known_unknown_fields".into(),
        json!({
            "state": "running",
            "health": "healthy",
            "engine_detail": "accepted and ignored"
        }),
    );
    fixtures.insert("capabilities".into(), Value::Array(capability_wires()));
    fixtures.insert("service_attempt".into(), to_value(&service_attempt()));
    fixtures.insert("deploy_intent".into(), to_value(&deploy_intent()));
    fixtures.insert("requested_service_spec".into(), to_value(&requested_spec()));
    fixtures.insert("deploy_preview".into(), to_value(&deploy_preview()));
    fixtures.insert(
        "deploy_preview_unknown_fields".into(),
        with_unknown_field(to_value(&deploy_preview()), "future_note", json!("ok")),
    );
    fixtures.insert(
        "deploy_event_progress".into(),
        to_value(&deploy_event_progress()),
    );
    fixtures.insert("deploy_outcome".into(), to_value(&deploy_outcome()));
    fixtures.insert(
        "deploy_outcome_unknown_fields".into(),
        with_unknown_field(to_value(&deploy_outcome()), "future_note", json!("ok")),
    );
    fixtures.insert(
        "deploy_outcome_failed".into(),
        to_value(&deploy_outcome_failed()),
    );
    fixtures.insert(
        "runtime_watch_frame".into(),
        to_value(&runtime_watch_frame()),
    );
    fixtures.insert(
        "runtime_watch_frame_unknown_fields".into(),
        with_unknown_field(
            to_value(&runtime_watch_frame()),
            "future_lens",
            json!({ "vendor": true }),
        ),
    );
    fixtures
}

pub(super) fn additive_examples() -> BTreeMap<&'static str, Value> {
    let frame = runtime_watch_frame();
    let machine_observation = frame
        .machines
        .first()
        .expect("RuntimeWatchFrame fixture includes a Machine Observation");
    let container = frame
        .containers
        .first()
        .expect("RuntimeWatchFrame fixture includes a Container Observation");
    let service = frame
        .services
        .first()
        .expect("RuntimeWatchFrame fixture includes a Service Observation");
    let certificate = frame
        .certificates
        .iter()
        .find(|row| row.backoff.is_some())
        .expect("RuntimeWatchFrame fixture includes a Certificate Observation with backoff");
    let rtt = machine_observation
        .rtt
        .as_ref()
        .expect("RuntimeWatchFrame fixture includes RTT");
    let partial = partial_result();
    BTreeMap::from([
        ("ContractDescription", to_value(&contract_description())),
        ("DockerVolume", to_value(&docker_volume())),
        ("DockerVolumeId", to_value(&docker_volume().id)),
        ("RemoveVolumesRequest", to_value(&remove_volumes_request())),
        ("ObservedDataLoss", to_value(&observed_data_loss())),
        ("UnconfirmedDataLoss", to_value(&unconfirmed_data_loss())),
        (
            "LocalMachineRemoved",
            to_value(&LocalMachineRemoved {
                reset_warning: Some("replicated delete failed".into()),
            }),
        ),
        ("DeployIntent", to_value(&deploy_intent())),
        ("DeployPreview", to_value(&deploy_preview())),
        ("PreservedVolume", to_value(&preserved_volume())),
        ("RequestedServiceSpec", to_value(&requested_spec())),
        ("ResolvedServiceSpec", to_value(&resolved_spec())),
        ("ServiceVolume", to_value(&service_volume())),
        ("ServiceMount", to_value(&service_mount())),
        ("Placement", to_value(&Placement::default())),
        ("UpdateConfig", to_value(&UpdateConfig::default())),
        (
            "ResolvedUpdateConfig",
            to_value(&ResolvedUpdateConfig::default()),
        ),
        ("ServiceContainerSpec", to_value(&resolved_spec().container)),
        (
            "OperationRow",
            to_value(
                deploy_preview()
                    .operations
                    .first()
                    .expect("preview fixture includes a row"),
            ),
        ),
        (
            "ContainerResources",
            to_value(&ContainerResources::default()),
        ),
        (
            "LogDriver",
            to_value(&LogDriver {
                name: "json-file".into(),
                options: BTreeMap::new(),
            }),
        ),
        (
            "PreDeployHook",
            to_value(&PreDeployHook {
                command: vec!["echo".into()],
                environment: BTreeMap::new(),
                privileged: None,
                timeout_millis: None,
                user: None,
            }),
        ),
        ("PlanOptions", to_value(&PlanOptions::default())),
        ("ServiceAttempt", to_value(&service_attempt())),
        ("RpcError", to_value(&rpc_error())),
        ("ReplacementOperation", to_value(&replacement_operation())),
        (
            "MachineSuccess",
            to_value(
                partial
                    .successes
                    .first()
                    .expect("partial_result fixture includes a success"),
            ),
        ),
        (
            "MachineFailure",
            to_value(
                partial
                    .failures
                    .first()
                    .expect("partial_result fixture includes a failure"),
            ),
        ),
        ("PartialResult", to_value(&partial)),
        (
            "MachineRuntime",
            to_value(&machine_observation.machine.runtime),
        ),
        ("Machine", to_value(&machine_observation.machine)),
        ("RttStatistics", to_value(rtt)),
        ("MachineObservation", to_value(machine_observation)),
        ("ContainerObservation", to_value(container)),
        ("ServiceObservation", to_value(service)),
        (
            "CertificateBackoff",
            to_value(certificate.backoff.as_ref().expect("backoff present")),
        ),
        ("CertificateObservation", to_value(certificate)),
        ("RuntimeWatchIncompleteIds", to_value(&frame.incomplete_ids)),
        ("RuntimeWatchFrame", to_value(&frame)),
    ])
}

pub(super) fn tagged_examples() -> BTreeMap<&'static str, Vec<Value>> {
    let DeployOutcome::Failed { failed, .. } = deploy_outcome_failed() else {
        panic!("failed fixture is Failed");
    };
    BTreeMap::from([
        ("DataLoss", vec![to_value(&data_loss())]),
        (
            "DeployOutcome",
            vec![
                to_value(&deploy_outcome()),
                to_value(&deploy_outcome_failed()),
            ],
        ),
        (
            "DeployOperation",
            deploy_operations().iter().map(to_value).collect(),
        ),
        (
            "ObservationKind",
            vec![
                to_value(&ObservationKind::Container),
                to_value(&ObservationKind::Volume),
            ],
        ),
        (
            "PruneRefusal",
            vec![
                to_value(&PruneRefusal::IncompleteSnapshot),
                to_value(&PruneRefusal::SelectedServices),
                to_value(&PruneRefusal::FilteredProfiles),
                to_value(&PruneRefusal::GuessedProjectName),
            ],
        ),
        (
            "DeployWarning",
            deploy_warnings().iter().map(to_value).collect(),
        ),
        (
            "MachineAction",
            vec![
                to_value(&MachineAction::CreateVolume),
                to_value(&MachineAction::CreateContainer),
                to_value(&MachineAction::StartContainer),
                to_value(&MachineAction::InspectContainer),
                to_value(&MachineAction::StopContainer),
                to_value(&MachineAction::RemoveContainer),
                to_value(&MachineAction::RemoveVolume),
            ],
        ),
        (
            "HealthFailure",
            vec![
                to_value(&HealthFailure::Cancelled),
                to_value(&HealthFailure::TimedOut),
                to_value(&HealthFailure::Runtime {
                    observation: ContainerRuntimeObservation::Restarting,
                }),
            ],
        ),
        (
            "HookFailure",
            vec![
                to_value(&HookFailure::Cancelled { stop_error: None }),
                to_value(&HookFailure::TimedOut {
                    stop_error: Some(rpc_error()),
                }),
                to_value(&HookFailure::Exit { code: 7 }),
            ],
        ),
        (
            "ExecutionError",
            vec![
                to_value(&execution_error_machine()),
                to_value(&ExecutionError::Health {
                    container_id: container_id(),
                    failure: HealthFailure::TimedOut,
                }),
                to_value(&ExecutionError::Hook {
                    container_id: container_id(),
                    failure: HookFailure::Exit { code: 1 },
                }),
                to_value(&ExecutionError::Cancelled),
            ],
        ),
        (
            "FailedOperation",
            vec![
                to_value(&failed),
                to_value(&FailedOperation::ReplacementHealth {
                    operation: replacement_operation(),
                    error: execution_error_machine(),
                    compensation: ReplacementCompensation::<ExecutionError>::StartFirst {
                        stop_new_container: Ok(()),
                    },
                }),
            ],
        ),
        (
            "RestartAttempt",
            vec![
                to_value(&RestartAttempt::<ExecutionError>::NotAttempted),
                to_value(&RestartAttempt::<ExecutionError>::Attempted(Ok(()))),
            ],
        ),
        (
            "ReplacementCompensation",
            vec![
                to_value(&ReplacementCompensation::<ExecutionError>::StartFirst {
                    stop_new_container: Ok(()),
                }),
                to_value(&ReplacementCompensation::<ExecutionError>::StopFirst {
                    stop_new_container: Ok(()),
                    restart_old_container: RestartAttempt::NotAttempted,
                }),
            ],
        ),
        (
            "ContainerKind",
            vec![
                to_value(&ContainerKind::ServiceContainer),
                to_value(&ContainerKind::PreDeployHook),
            ],
        ),
        (
            "ContainerRuntimeObservation",
            vec![
                to_value(&ContainerRuntimeObservation::Created),
                to_value(&ContainerRuntimeObservation::Running {
                    health: HealthObservation::Healthy,
                }),
                to_value(&ContainerRuntimeObservation::Paused),
                to_value(&ContainerRuntimeObservation::Restarting),
                to_value(&ContainerRuntimeObservation::Exited { code: 0 }),
                to_value(&ContainerRuntimeObservation::Removing),
                to_value(&ContainerRuntimeObservation::Dead),
            ],
        ),
        (
            "ServiceMode",
            vec![
                to_value(&ServiceMode::Replicated {
                    replicas: NonZeroU32::MIN,
                }),
                to_value(&ServiceMode::Global),
            ],
        ),
        (
            "PortPublication",
            vec![to_value(&ingress_port()), to_value(&host_port())],
        ),
        (
            "VolumeSource",
            vec![
                to_value(&VolumeSource::Bind {
                    machine_path: MachinePath::parse("/data").expect("fixture bind path is valid"),
                    create_machine_path: false,
                    propagation: Some(BindPropagation::Private),
                    recursive: Some(BindRecursive::Disabled),
                }),
                to_value(&service_volume().source),
                to_value(&VolumeSource::Tmpfs {
                    size_bytes: Some(64),
                    mode: Some(0o755),
                    options: Vec::new(),
                }),
            ],
        ),
        (
            "IngressHostname",
            vec![
                to_value(&IngressHostname::AssignFromClusterDomain),
                to_value(&IngressHostname::Explicit {
                    hostname: ingress_host("app.example.com"),
                }),
            ],
        ),
        (
            "HostBind",
            vec![
                to_value(&HostBind::All),
                to_value(&HostBind::Address {
                    address: IpAddr::from([127, 0, 0, 1]),
                }),
                to_value(
                    &serde_json::from_value::<HostBind>(
                        json!({ "kind": "prefix", "prefix": "10.0.0.0/8" }),
                    )
                    .expect("fixture HostBind prefix is valid"),
                ),
            ],
        ),
        (
            "HealthcheckSpec",
            vec![
                to_value(&HealthcheckSpec::Disabled),
                to_value(&HealthcheckSpec::Configured(ConfiguredHealthcheck {
                    test: HealthcheckCommand::parse(["CMD", "true"])
                        .expect("fixture healthcheck command is valid"),
                    interval_millis: None,
                    timeout_millis: None,
                    start_period_millis: None,
                    start_interval_millis: None,
                    retries: None,
                })),
            ],
        ),
        (
            "RestartPolicy",
            vec![
                to_value(&RestartPolicy::No),
                to_value(&RestartPolicy::Always),
                to_value(&RestartPolicy::UnlessStopped),
                to_value(&RestartPolicy::OnFailure {
                    maximum_retry_count: Some(2),
                }),
            ],
        ),
        (
            "PullPolicy",
            vec![
                to_value(&PullPolicy::Always),
                to_value(&PullPolicy::Missing),
                to_value(&PullPolicy::Never),
            ],
        ),
        (
            "UpdateOrder",
            vec![
                to_value(&UpdateOrder::StartFirst),
                to_value(&UpdateOrder::StopFirst),
            ],
        ),
        (
            "HttpProtocol",
            vec![
                to_value(&HttpProtocol::Http),
                to_value(&HttpProtocol::Https),
            ],
        ),
        (
            "TransportProtocol",
            vec![
                to_value(&TransportProtocol::Tcp),
                to_value(&TransportProtocol::Udp),
            ],
        ),
        (
            "DeployEvent",
            vec![
                to_value(&deploy_event_progress()),
                to_value(&DeployEvent::Outcome {
                    outcome: deploy_outcome(),
                }),
            ],
        ),
        (
            "OperationStatus",
            operation_statuses().iter().map(to_value).collect(),
        ),
        (
            "OperationPhase",
            operation_phases().iter().map(to_value).collect(),
        ),
    ])
}

pub(super) fn catalogued_capabilities() -> Vec<(&'static str, &'static str)> {
    let mut rows = ployz_core::CATALOGUED_CAPABILITY_BINDINGS.to_vec();
    rows.sort_by_key(|(_, wire)| *wire);
    rows
}

fn capability_wires() -> Vec<Value> {
    catalogued_capabilities()
        .into_iter()
        .map(|(_, name)| Value::String(name.to_owned()))
        .collect()
}

fn contract_description() -> ContractDescription {
    ContractDescription {
        machine_id: machine_id(MACHINE_ID_HEX),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "0.1.0".into(),
        capabilities: [CapabilityName::parse(DESCRIBE_CONTRACT_CAPABILITY)
            .expect("catalogued capability names are valid")]
        .into(),
    }
}

fn rpc_error() -> RpcError {
    RpcError {
        code: RpcErrorCode::Unsupported,
        message: "watch is not advertised".into(),
        details: Value::Null,
    }
}

fn docker_volume() -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id: machine_id(MACHINE_ID_HEX),
            name: DockerVolumeName::parse("data").expect("fixture volume name is valid"),
        },
        driver: "local".into(),
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("ployz.managed".into(), "false".into())]),
    }
}

fn remove_volumes_request() -> RemoveVolumesRequest {
    RemoveVolumesRequest {
        volumes: vec![docker_volume().id],
        force: false,
    }
}

fn data_loss() -> DataLoss {
    DataLoss::DockerVolume(docker_volume().id)
}

fn observed_data_loss() -> ObservedDataLoss {
    ObservedDataLoss {
        data_loss: vec![data_loss()],
    }
}

fn unconfirmed_data_loss() -> UnconfirmedDataLoss {
    UnconfirmedDataLoss {
        missing: vec![data_loss()],
    }
}

fn service_attempt() -> ServiceAttempt {
    ServiceAttempt {
        name: ServiceName::parse("web").expect("fixture Service Name is valid"),
    }
}

fn deploy_intent() -> DeployIntent {
    DeployIntent::new(
        ProjectName::parse("app").unwrap(),
        Vec::new(),
        PlanOptions::default(),
    )
}

fn deploy_preview() -> DeployPreview {
    DeployPreview::new(
        vec![OperationRow::pending(
            0,
            DeployOperation::StopContainer {
                machine_id: machine_id(MACHINE_ID_HEX),
                container_id: container_id(),
            },
            Some(MachineName::parse("edge").expect("fixture Machine Name is valid")),
            Some("api-1".into()),
            Some(ServiceName::parse("api").expect("fixture Service Name is valid")),
        )],
        deploy_warnings().to_vec(),
        ProjectName::parse("app").unwrap(),
    )
}

fn preserved_volume() -> PreservedVolume {
    PreservedVolume {
        id: docker_volume().id,
        machine_name: Some(MachineName::parse("edge").expect("fixture Machine Name is valid")),
    }
}

fn deploy_event_progress() -> DeployEvent {
    DeployEvent::Progress {
        completed: 0,
        total: 1,
        rows: deploy_preview().operations,
    }
}

fn deploy_warnings() -> [DeployWarning; 4] {
    [
        DeployWarning::ObservationFailed {
            kind: ObservationKind::Container,
            machine_id: machine_id(MACHINE_ID_HEX),
            message: "container listing failed".into(),
        },
        DeployWarning::ObservationOmitted {
            kind: ObservationKind::Volume,
            machine_id: machine_id(OTHER_MACHINE_ID_HEX),
        },
        DeployWarning::IngressHostname(
            "Ingress Hostname app.example.com does not resolve; it should resolve to 192.0.2.1."
                .into(),
        ),
        DeployWarning::ObserverRelativeHostnameConflict,
    ]
}

fn deploy_outcome() -> DeployOutcome<ExecutionError> {
    DeployOutcome::Success {
        completed: vec![DeployOperation::StopContainer {
            machine_id: machine_id(MACHINE_ID_HEX),
            container_id: container_id(),
        }],
    }
}

fn deploy_outcome_failed() -> DeployOutcome<ExecutionError> {
    DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Operation {
            operation: DeployOperation::CreateVolume {
                machine_id: machine_id(MACHINE_ID_HEX),
                volume: service_volume(),
            },
            error: execution_error_machine(),
        },
        unexecuted: vec![DeployOperation::StopContainer {
            machine_id: machine_id(MACHINE_ID_HEX),
            container_id: container_id(),
        }],
    }
}

fn execution_error_machine() -> ExecutionError {
    ExecutionError::Machine {
        action: MachineAction::CreateVolume,
        error: rpc_error(),
    }
}

fn replacement_operation() -> ReplacementOperation {
    ReplacementOperation {
        machine_id: machine_id(MACHINE_ID_HEX),
        old_container_id: container_id(),
        spec: resolved_spec(),
        skip_health_monitor: false,
    }
}

fn deploy_operations() -> [DeployOperation; 8] {
    let machine_id = machine_id(MACHINE_ID_HEX);
    let container_id = container_id();
    [
        DeployOperation::CreateVolume {
            machine_id,
            volume: service_volume(),
        },
        DeployOperation::RunContainer {
            machine_id,
            spec: resolved_spec(),
            skip_health_monitor: false,
        },
        DeployOperation::StopContainer {
            machine_id,
            container_id,
        },
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
        },
        DeployOperation::ReplaceContainer(replacement_operation()),
        DeployOperation::StopHook {
            machine_id,
            container_id,
        },
        DeployOperation::RunHook {
            machine_id,
            spec: resolved_spec(),
            old_hook_containers: vec![(machine_id, container_id)],
        },
        DeployOperation::RemoveVolume {
            id: DockerVolumeId {
                machine_id,
                name: DockerVolumeName::parse("data").expect("fixture volume name is valid"),
            },
        },
    ]
}

fn service_volume() -> ServiceVolume {
    ServiceVolume {
        reference: ServiceVolumeReference::parse("data")
            .expect("fixture volume reference is valid"),
        source: VolumeSource::Named {
            name: DockerVolumeName::parse("data").expect("fixture volume name is valid"),
            external: false,
            driver: None,
            labels: BTreeMap::new(),
            no_copy: false,
            subpath: None,
        },
    }
}

fn runtime_watch_frame() -> RuntimeWatchFrame {
    let container = container_observation();
    RuntimeWatchFrame {
        machines: vec![MachineObservation {
            machine: Machine {
                id: machine_id(MACHINE_ID_HEX),
                name: MachineName::parse("edge").expect("fixture Machine Name is valid"),
                subnet: "10.210.1.0/24"
                    .parse()
                    .expect("fixture Machine Subnet is valid"),
                management_address: ManagementAddress(
                    "::1"
                        .parse::<Ipv6Addr>()
                        .expect("fixture management address is valid"),
                ),
                public_key: WireGuardPublicKey([0; 32]),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint(endpoint())],
                runtime: MachineRuntime {
                    daemon_version: "0.1.0".into(),
                    docker_version: "27.0.0".into(),
                    hostname: "edge".into(),
                    architecture: "x86_64".into(),
                    os_pretty_name: "Debian".into(),
                    kernel_version: "6.1.0".into(),
                },
            },
            membership: MembershipObservation::Up,
            selected_endpoint: Some(SelectedEndpoint(endpoint())),
            rtt: Some(RttStatistics {
                median_ns: 1_500_000,
                population_stddev_ns: 250_000,
            }),
        }],
        containers: vec![container.clone()],
        services: vec![ServiceObservation {
            identity: container.identity(),
            service_id: service_id(),
            containers: vec![ServiceContainer::try_from(container)
                .expect("fixture container is a Service Container")],
            hook_containers: Vec::<HookContainer>::new(),
        }],
        volumes: vec![docker_volume()],
        certificates: vec![
            CertificateObservation {
                hostname: ingress_host("ok.example.com"),
                status: CertificateAvailability::Available,
                last_error: None,
                backoff: None,
            },
            CertificateObservation {
                hostname: ingress_host("app.example.com"),
                status: CertificateAvailability::Failure,
                last_error: Some(
                    "Ingress Hostname app.example.com does not resolve; it should resolve to 192.0.2.1."
                        .into(),
                ),
                backoff: Some(CertificateBackoff {
                    failure_kind: CertificateFailureKind::DoesNotResolve,
                    next_attempt_at: "2024-01-01T01:00:00Z".into(),
                    failures: 2,
                }),
            },
        ],
        hosted_dns_hostname: Some("cluster.example.ts.net".into()),
        incomplete_ids: RuntimeWatchIncompleteIds {
            machines: vec![machine_id(OTHER_MACHINE_ID_HEX)],
            containers: vec![ContainerId::parse(INCOMPLETE_CONTAINER_ID_HEX)
                .expect("fixture incomplete Container ID is valid")],
            volumes: vec![DockerVolumeId {
                machine_id: machine_id(OTHER_MACHINE_ID_HEX),
                name: DockerVolumeName::parse("scratch").expect("fixture volume name is valid"),
            }],
            certificates: vec![ingress_host("pending.example.com")],
        },
        observed_at: "2024-01-01T00:00:00Z".into(),
    }
}

fn container_observation() -> ContainerObservation {
    ContainerObservation {
        container_id: ContainerId::parse("1".repeat(64)).expect("fixture Container ID is valid"),
        display_name: "api-1".into(),
        created_at_unix_nanos: 1_700_000_000_000_000_000,
        machine_id: machine_id(MACHINE_ID_HEX),
        project_name: ProjectName::parse("app").unwrap(),
        service_id: service_id(),
        service_name: ServiceName::parse("api").expect("fixture Service Name is valid"),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec: resolved_spec(),
        address: None,
        labels: BTreeMap::new(),
    }
}

fn requested_spec() -> RequestedServiceSpec {
    serde_json::from_value(json!({
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "api:1", "pull_policy": "missing" }
    }))
    .expect("fixture Requested Service Spec is valid")
}

fn resolved_spec() -> ResolvedServiceSpec {
    serde_json::from_value(json!({
        "service_id": SERVICE_ID_HEX,
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "api:1", "pull_policy": "missing" }
    }))
    .expect("fixture Resolved Service Spec is valid")
}

fn service_mount() -> ServiceMount {
    ServiceMount {
        volume: ServiceVolumeReference::parse("data").expect("fixture volume reference is valid"),
        target: ContainerPath::parse("/data").expect("fixture container path is valid"),
        read_only: false,
    }
}

fn ingress_port() -> PortPublication {
    PortPublication::Ingress {
        hostname: IngressHostname::Explicit {
            hostname: ingress_host("app.example.com"),
        },
        load_balancer_port: NonZeroU16::new(443).expect("port is non-zero"),
        container_port: NonZeroU16::new(80).expect("port is non-zero"),
        http_protocol: HttpProtocol::Https,
    }
}

fn host_port() -> PortPublication {
    PortPublication::Host {
        bind: HostBind::All,
        published_port: NonZeroU16::new(8080).expect("port is non-zero"),
        container_port: NonZeroU16::new(80).expect("port is non-zero"),
        transport_protocol: TransportProtocol::Tcp,
    }
}

fn operation_statuses() -> [OperationStatus; 5] {
    [
        OperationStatus::Pending,
        OperationStatus::Running {
            phase: OperationPhase::Starting,
        },
        OperationStatus::Completed,
        OperationStatus::Failed {
            error: ExecutionError::Cancelled,
        },
        OperationStatus::Unexecuted,
    ]
}

fn operation_phases() -> [OperationPhase; 10] {
    [
        OperationPhase::Starting,
        OperationPhase::CreatingVolume,
        OperationPhase::CreatingContainer,
        OperationPhase::StartingContainer,
        OperationPhase::WaitingForHealth {
            container_id: container_id(),
            health: Some(HealthObservation::Starting),
            elapsed_ms: 1_200,
            deadline_ms: 60_000,
        },
        OperationPhase::WaitingForHook {
            container_id: container_id(),
            elapsed_ms: 400,
            deadline_ms: 300_000,
        },
        OperationPhase::StoppingContainer,
        OperationPhase::RemovingContainer,
        OperationPhase::RemovingVolume,
        OperationPhase::Compensating,
    ]
}

fn container_id() -> ContainerId {
    ContainerId::parse(CONTAINER_ID_HEX).expect("fixture Container ID is valid")
}

fn partial_result() -> PartialResult<DockerVolume, RpcError> {
    PartialResult {
        successes: vec![MachineSuccess {
            machine_id: machine_id(MACHINE_ID_HEX),
            value: docker_volume(),
        }],
        failures: vec![MachineFailure {
            machine_id: machine_id(OTHER_MACHINE_ID_HEX),
            error: rpc_error(),
        }],
        omissions: vec![machine_id("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")],
    }
}

fn machine_id(value: &str) -> MachineId {
    MachineId::parse(value).expect("fixture Machine ID is valid")
}

fn service_id() -> ServiceId {
    ServiceId::parse(SERVICE_ID_HEX).expect("fixture Service ID is valid")
}

fn ingress_host(value: &str) -> IngressHost {
    IngressHost::parse(value).expect("fixture Ingress Hostname is valid")
}

fn endpoint() -> SocketAddr {
    "203.0.113.10:51820"
        .parse()
        .expect("fixture endpoint is valid")
}

fn to_value<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).expect("SDK fixtures serialize")
}

fn with_unknown_field(mut value: Value, key: &str, extra: Value) -> Value {
    value
        .as_object_mut()
        .expect("unknown-field fixtures wrap objects")
        .insert(key.to_owned(), extra);
    value
}
