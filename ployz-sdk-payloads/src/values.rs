//! Rust values that become JSON fixtures and Additive serde examples.

use std::{
    collections::BTreeMap,
    net::{Ipv6Addr, SocketAddr},
};

use ployz_core::{
    AdvertisedEndpoint, CapabilityName, CertificateAvailability, CertificateBackoff,
    CertificateFailureKind, CertificateObservation, ContainerId, ContainerKind,
    ContainerObservation, ContainerRuntimeObservation, ContractDescription,
    DESCRIBE_CONTRACT_CAPABILITY, DataLoss, DeployIntent, DeployOperation, DeployOutcome,
    DeployPreview, DeployWarning, DockerVolume, DockerVolumeId, DockerVolumeName, ExecutionError,
    FailedOperation, HealthFailure, HealthObservation, HookContainer, HookFailure, IngressHost,
    Machine, MachineAction, MachineFailure, MachineId, MachineName, MachineObservation,
    MachineRuntime, MachineSuccess, ManagementAddress, MembershipObservation, ObservationKind,
    ObservedDataLoss, PROTOCOL_MAJOR, PartialResult, PlanOptions, RemoveVolumesRequest,
    ReplacementCompensation, ReplacementOperation, ResolvedServiceSpec, RestartAttempt, RpcError,
    RpcErrorCode, RttStatistics, RuntimeWatchFrame, RuntimeWatchIncompleteIds, SelectedEndpoint,
    ServiceAttempt, ServiceContainer, ServiceId, ServiceName, ServiceObservation, ServiceVolume,
    ServiceVolumeReference, VolumeSource, WireGuardPublicKey,
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
    fixtures.insert("deploy_preview".into(), to_value(&deploy_preview()));
    fixtures.insert(
        "deploy_preview_unknown_fields".into(),
        with_unknown_field(to_value(&deploy_preview()), "future_note", json!("ok")),
    );
    fixtures.insert("deploy_outcome".into(), to_value(&deploy_outcome()));
    fixtures.insert(
        "deploy_outcome_unknown_fields".into(),
        with_unknown_field_in(
            to_value(&deploy_outcome()),
            "Success",
            "future_note",
            json!("ok"),
        ),
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
        ("DeployIntent", to_value(&deploy_intent())),
        ("DeployPreview", to_value(&deploy_preview())),
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
            ],
        ),
        (
            "HealthFailure",
            vec![
                to_value(&HealthFailure::Cancelled),
                to_value(&HealthFailure::TimedOut),
                to_value(&HealthFailure::Runtime(
                    ContainerRuntimeObservation::Restarting,
                )),
            ],
        ),
        (
            "HookFailure",
            vec![
                to_value(&HookFailure::Cancelled { stop_error: None }),
                to_value(&HookFailure::TimedOut {
                    stop_error: Some(rpc_error()),
                }),
                to_value(&HookFailure::Exit(7)),
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
                    failure: HookFailure::Exit(1),
                }),
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

fn service_attempt() -> ServiceAttempt {
    ServiceAttempt {
        name: ServiceName::parse("web").expect("fixture Service Name is valid"),
    }
}

fn deploy_intent() -> DeployIntent {
    DeployIntent::new(Vec::new(), Vec::new(), PlanOptions::default())
}

fn deploy_preview() -> DeployPreview {
    DeployPreview {
        operations: vec![DeployOperation::StopContainer {
            machine_id: machine_id(MACHINE_ID_HEX),
            container_id: container_id(),
        }],
        warnings: deploy_warnings().to_vec(),
    }
}

fn deploy_warnings() -> [DeployWarning; 3] {
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

fn deploy_operations() -> [DeployOperation; 7] {
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

fn resolved_spec() -> ResolvedServiceSpec {
    serde_json::from_value(json!({
        "service_id": SERVICE_ID_HEX,
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "api:1", "pull_policy": "missing" }
    }))
    .expect("fixture Resolved Service Spec is valid")
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

fn with_unknown_field_in(mut value: Value, variant: &str, key: &str, extra: Value) -> Value {
    value
        .get_mut(variant)
        .and_then(Value::as_object_mut)
        .expect("unknown-field fixtures wrap a serde variant object")
        .insert(key.to_owned(), extra);
    value
}
