use std::collections::BTreeSet;

use ployz_core::{
    AdvertisedEndpoint, CertificateAvailability, CertificateBackoff, CertificateFailureKind,
    CertificateObservation, CodecError, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, ContractDescription, DockerVolume, DockerVolumeId,
    DockerVolumeName, HealthObservation, IngressHost, Machine, MachineId, MachineName,
    MachineObservation, MachineRuntime, ManagementAddress, MembershipObservation, OpaquePayload,
    PROTOCOL_MAJOR, ProjectName, RUNTIME_WATCH_CAPABILITY, ResolvedServiceSpec, RpcRequestBody,
    RttStatistics, RuntimeWatchFrame, RuntimeWatchIncompleteIds, RuntimeWatchPayloadError,
    RuntimeWatchRequest, SelectedEndpoint, ServiceContainer, ServiceId, ServiceName,
    ServiceObservation, WireGuardPublicKey, decode_runtime_watch_frame, encode_runtime_watch_frame,
    op,
};
use serde_json::{Value, json};

const MACHINE_ID: &str = "0123456789abcdef0123456789abcdef";
const OTHER_MACHINE_ID: &str = "fedcba9876543210fedcba9876543210";
const SERVICE_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTAINER_ID: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const INCOMPLETE_CONTAINER_ID: &str =
    "2222222222222222222222222222222222222222222222222222222222222222";
const FROZEN_FRAME: &str = include_str!("fixtures/runtime_watch_frame.json");

#[test]
fn runtime_watch_capability_is_always_advertised_and_not_inferred_from_daemon_version() {
    let old_daemon = ContractDescription {
        machine_id: MachineId::parse(MACHINE_ID).unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "9.9.9".into(),
        capabilities: BTreeSet::new(),
    };
    assert!(!old_daemon.supports(RUNTIME_WATCH_CAPABILITY));

    let new_daemon = ContractDescription {
        machine_id: MachineId::parse(MACHINE_ID).unwrap(),
        protocol_major: PROTOCOL_MAJOR,
        daemon_version: "0.0.1".into(),
        capabilities: BTreeSet::from([
            ployz_core::CapabilityName::parse(RUNTIME_WATCH_CAPABILITY).unwrap()
        ]),
    };
    assert!(new_daemon.supports(RUNTIME_WATCH_CAPABILITY));
}

#[test]
fn runtime_watch_request_is_empty_and_catalogued() {
    let encoded = op::RuntimeWatch::into_request(RuntimeWatchRequest {})
        .encode()
        .unwrap();
    assert_eq!(
        encoded.decode_request().unwrap().body,
        RpcRequestBody::RuntimeWatch(RuntimeWatchRequest {})
    );
}

#[test]
fn frozen_runtime_watch_frame_round_trips_complete_observations() {
    let frame: RuntimeWatchFrame = serde_json::from_str(FROZEN_FRAME).unwrap();
    assert_eq!(frame, expected_frame());
    let encoded = encode_runtime_watch_frame(&frame).unwrap();
    let wire: Value = encoded.decode_json().unwrap();
    assert!(wire.get("services").is_none());
    assert_eq!(decode_runtime_watch_frame(&encoded).unwrap(), frame);
}

#[test]
fn runtime_watch_payload_rejects_malformed_json() {
    let error = decode_runtime_watch_frame(&OpaquePayload::new(b"{".to_vec())).unwrap_err();
    assert!(matches!(
        error,
        RuntimeWatchPayloadError::Codec(CodecError::DecodeJson(_))
    ));
}

#[test]
fn frozen_runtime_watch_frame_never_carries_certificate_material_or_dns_credentials() {
    let fixture: Value = serde_json::from_str(FROZEN_FRAME).unwrap();
    let serialized = serde_json::to_value(expected_frame()).unwrap();
    for payload in [&fixture, &serialized] {
        assert_no_secret_material(payload);
    }
}

#[test]
fn runtime_watch_frame_accepts_unknown_fields_and_honest_defaults() {
    let mut additive: Value = serde_json::from_str(FROZEN_FRAME).unwrap();
    additive
        .as_object_mut()
        .expect("frame object")
        .insert("future_lens".into(), json!({ "vendor": true }));
    insert_json_field(
        &mut additive,
        "/certificates/0",
        "certificate",
        json!("-----BEGIN CERTIFICATE-----\n"),
    );
    insert_json_field(
        &mut additive,
        "/certificates/0",
        "private_key",
        json!("-----BEGIN PRIVATE KEY-----\n"),
    );
    insert_json_field(
        &mut additive,
        "/certificates/2",
        "challenge_token",
        json!("http-01-token"),
    );

    let decoded = serde_json::from_value::<RuntimeWatchFrame>(additive).unwrap();
    assert_eq!(decoded, expected_frame());
    let redacted = serde_json::to_value(&decoded).unwrap();
    assert!(redacted.get("future_lens").is_none());
    assert_no_secret_material(&redacted);

    let defaults = serde_json::from_value::<RuntimeWatchFrame>(json!({
        "observed_at": "2024-01-01T00:00:00Z"
    }))
    .unwrap();
    assert!(defaults.machines.is_empty());
    assert!(defaults.containers.is_empty());
    assert!(defaults.services.is_empty());
    assert!(defaults.volumes.is_empty());
    assert!(defaults.certificates.is_empty());
    assert_eq!(defaults.hosted_dns_hostname, None);
    assert_eq!(
        defaults.incomplete_ids,
        RuntimeWatchIncompleteIds::default()
    );
    assert_eq!(defaults.observed_at, "2024-01-01T00:00:00Z");
}

#[test]
fn observation_enums_keep_an_unknown_case() {
    let status: CertificateAvailability = serde_json::from_str("\"renewing\"").unwrap();
    assert_eq!(
        status,
        CertificateAvailability::Unrecognized("renewing".into())
    );
    assert_eq!(serde_json::to_string(&status).unwrap(), "\"renewing\"");

    let kind: CertificateFailureKind = serde_json::from_str("\"rate_limited\"").unwrap();
    assert_eq!(
        kind,
        CertificateFailureKind::Unrecognized("rate_limited".into())
    );

    let cert: CertificateObservation = serde_json::from_value(json!({
        "hostname": "app.example.com",
        "status": "renewing",
        "issuer": "future"
    }))
    .unwrap();
    assert_eq!(cert.hostname.as_str(), "app.example.com");
    assert_eq!(
        cert.status,
        CertificateAvailability::Unrecognized("renewing".into())
    );
    assert_eq!(cert.last_error, None);
    assert_eq!(cert.backoff, None);
}

fn insert_json_field(payload: &mut Value, pointer: &str, key: &str, value: Value) {
    payload
        .pointer_mut(pointer)
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("{pointer}"))
        .insert(key.into(), value);
}

fn assert_no_secret_material(payload: &Value) {
    let text = payload.to_string();
    for forbidden in [
        "BEGIN CERTIFICATE",
        "BEGIN PRIVATE KEY",
        "private_key",
        "challenge_token",
        "challenge_response",
        "renewal_token",
        "dns_endpoint",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} must not appear on the Watch frame"
        );
    }
}

fn expected_frame() -> RuntimeWatchFrame {
    let container = container_observation();
    RuntimeWatchFrame {
        machines: vec![MachineObservation {
            selected_endpoint: Some(SelectedEndpoint("203.0.113.10:51820".parse().unwrap())),
            rtt: Some(RttStatistics {
                median_ns: 1_500_000,
                population_stddev_ns: 250_000,
            }),
            ..MachineObservation::new(
                Machine {
                id: MachineId::parse(MACHINE_ID).unwrap(),
                name: MachineName::parse("edge").unwrap(),
                subnet: "10.210.1.0/24".parse().unwrap(),
                management_address: ManagementAddress("::1".parse().unwrap()),
                public_key: WireGuardPublicKey([0; 32]),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("203.0.113.10:51820".parse().unwrap())],
                runtime: MachineRuntime {
                    daemon_version: "0.1.0".into(),
                    docker_version: "27.0.0".into(),
                    hostname: "edge".into(),
                    architecture: "x86_64".into(),
                    os_pretty_name: "Debian".into(),
                    kernel_version: "6.1.0".into(),
                },
                },
                MembershipObservation::Up,
            )
        }],
        containers: vec![container.clone()],
        services: vec![ServiceObservation {
            identity: container.identity(),
            service_id: ServiceId::parse(SERVICE_ID).unwrap(),
            containers: vec![ServiceContainer::try_from(container).unwrap()],
            hook_containers: Vec::new(),
        }],
        volumes: vec![DockerVolume {
            id: DockerVolumeId {
                machine_id: MachineId::parse(MACHINE_ID).unwrap(),
                name: DockerVolumeName::parse("data").unwrap(),
            },
            options: [("type".into(), "none".into())].into(),
            labels: [("purpose".into(), "database".into())].into(),
            storage: ployz_core::DockerVolumeStorageObservation::Plain {
                driver: "local".into(),
            },
        }],
        certificates: vec![
            CertificateObservation {
                hostname: IngressHost::parse("ok.example.com").unwrap(),
                status: CertificateAvailability::Available,
                last_error: None,
                backoff: None,
            },
            CertificateObservation {
                hostname: IngressHost::parse("new.example.com").unwrap(),
                status: CertificateAvailability::Pending,
                last_error: None,
                backoff: None,
            },
            CertificateObservation {
                hostname: IngressHost::parse("app.example.com").unwrap(),
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
            CertificateObservation {
                hostname: IngressHost::parse("maybe.example.com").unwrap(),
                status: CertificateAvailability::Unknown,
                last_error: None,
                backoff: None,
            },
        ],
        hosted_dns_hostname: Some("cluster.example.ts.net".into()),
        incomplete_ids: RuntimeWatchIncompleteIds {
            machines: vec![MachineId::parse(OTHER_MACHINE_ID).unwrap()],
            containers: vec![ContainerId::parse(INCOMPLETE_CONTAINER_ID).unwrap()],
            volumes: vec![DockerVolumeId {
                machine_id: MachineId::parse(OTHER_MACHINE_ID).unwrap(),
                name: DockerVolumeName::parse("scratch").unwrap(),
            }],
            certificates: vec![IngressHost::parse("pending.example.com").unwrap()],
        },
        observed_at: "2024-01-01T00:00:00Z".into(),
    }
}

fn container_observation() -> ContainerObservation {
    let service_id = ServiceId::parse(SERVICE_ID).unwrap();
    let service_name = ServiceName::parse("api").unwrap();
    let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": service_id,
        "name": service_name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "api:1", "pull_policy": "missing" }
    }))
    .unwrap();
    ContainerObservation {
        container_id: ContainerId::parse(CONTAINER_ID).unwrap(),
        display_name: "api-1".into(),
        created_at_unix_nanos: 1_700_000_000_000_000_000,
        machine_id: MachineId::parse(MACHINE_ID).unwrap(),
        project_name: ProjectName::parse("app").unwrap(),
        service_id,
        service_name,
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec,
        address: None,
        labels: Default::default(),
    }
}
