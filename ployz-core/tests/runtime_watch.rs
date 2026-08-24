use std::collections::BTreeSet;

use ployz_core::{
    AdvertisedEndpoint, CertificateAvailability, CertificateBackoff, CertificateFailureKind,
    CertificateObservation, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, ContractDescription, DockerVolume, DockerVolumeId,
    DockerVolumeName, HealthObservation, IngressHost, Machine, MachineId, MachineName,
    MachineObservation, MachineRuntime, ManagementAddress, MembershipObservation, OpaquePayload,
    PROTOCOL_MAJOR, ProjectName, RUNTIME_WATCH_CAPABILITY, ResolvedServiceSpec, RpcRequestBody,
    RttStatistics, RuntimeWatchFrame, RuntimeWatchIncompleteIds, RuntimeWatchRequest,
    RuntimeWatchTransportFrame, SelectedEndpoint, ServiceContainer, ServiceId, ServiceName,
    ServiceObservation, WireGuardPublicKey, derive_services, op,
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
    let transport: RuntimeWatchTransportFrame = serde_json::from_str(FROZEN_FRAME).unwrap();
    let frame = transport.into_frame();
    assert_eq!(frame, expected_frame());
    let encoded =
        OpaquePayload::from_json(&RuntimeWatchTransportFrame::from_frame(&frame)).unwrap();
    assert_eq!(
        encoded
            .decode_json::<RuntimeWatchTransportFrame>()
            .unwrap()
            .into_frame(),
        frame
    );
}

#[test]
fn frozen_runtime_watch_frame_never_carries_certificate_material_or_dns_credentials() {
    let fixture: Value = serde_json::from_str(FROZEN_FRAME).unwrap();
    let serialized = transport_value(&expected_frame());
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

    let decoded = serde_json::from_value::<RuntimeWatchTransportFrame>(additive)
        .unwrap()
        .into_frame();
    assert_eq!(decoded, expected_frame());
    let redacted = transport_value(&decoded);
    assert!(redacted.get("future_lens").is_none());
    assert_no_secret_material(&redacted);

    let defaults = serde_json::from_value::<RuntimeWatchTransportFrame>(json!({
        "observed_at": "2024-01-01T00:00:00Z"
    }))
    .unwrap()
    .into_frame();
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
fn normalized_transport_deduplicates_complete_specs_and_container_rows() {
    let mut frame = replicated_frame(4);
    frame
        .containers
        .get_mut(3)
        .unwrap()
        .resolved_spec
        .container
        .image = "api:2".into();
    frame.services = derive_services(frame.containers.iter().cloned());

    let transport = transport_value(&frame);
    assert_eq!(json_array(&transport, "/specs").len(), 2);
    assert_eq!(json_array(&transport, "/containers").len(), 4);
    assert_eq!(json_array(&transport, "/services").len(), 1);
    assert_eq!(json_array(&transport, "/services/0/member_ids").len(), 4);
    assert!(transport.pointer("/containers/0/resolved_spec").is_none());
    assert!(transport.pointer("/services/0/containers").is_none());

    let decoded = serde_json::from_value::<RuntimeWatchTransportFrame>(transport)
        .unwrap()
        .into_frame();
    assert_eq!(
        decoded
            .containers
            .first()
            .unwrap()
            .resolved_spec
            .container
            .image,
        "api:1"
    );
    assert_eq!(
        decoded
            .containers
            .get(3)
            .unwrap()
            .resolved_spec
            .container
            .image,
        "api:2"
    );
    assert_eq!(decoded.services.first().unwrap().members().count(), 4);
}

#[test]
fn normalized_transport_is_canonical_and_keeps_rollout_specs_lossless() {
    let mut forward = replicated_frame(3);
    let changed = forward.containers.get_mut(1).unwrap();
    changed.resolved_spec.container.image = "api:2".into();
    changed.created_at_unix_nanos += 10;
    forward.services = derive_services(forward.containers.iter().cloned());
    let mut reversed = forward.clone();
    reversed.containers.reverse();
    reversed.services.reverse();

    let forward_json =
        serde_json::to_vec(&RuntimeWatchTransportFrame::from_frame(&forward)).unwrap();
    let reversed_json =
        serde_json::to_vec(&RuntimeWatchTransportFrame::from_frame(&reversed)).unwrap();
    assert_eq!(forward_json, reversed_json);

    let decoded = serde_json::from_slice::<RuntimeWatchTransportFrame>(&forward_json)
        .unwrap()
        .into_frame();
    assert_eq!(
        decoded
            .containers
            .get(1)
            .unwrap()
            .resolved_spec
            .container
            .image,
        "api:2"
    );
    assert!(
        decoded
            .containers
            .iter()
            .all(|container| container.service_id == ServiceId::parse(SERVICE_ID).unwrap())
    );
}

#[test]
fn malformed_normalized_graphs_fail_as_whole_frames() {
    let valid = transport_value(&expected_frame());

    let mut broken_spec = valid.clone();
    *json_mut(&mut broken_spec, "/containers/0/spec_index") = json!(99);
    assert_invalid_transport(broken_spec);

    let mut duplicate_spec = valid.clone();
    let spec = duplicate_spec.pointer("/specs/0").unwrap().clone();
    json_array_mut(&mut duplicate_spec, "/specs").push(spec);
    assert_invalid_transport(duplicate_spec);

    let mut duplicate_container = valid.clone();
    let container = duplicate_container
        .pointer("/containers/0")
        .unwrap()
        .clone();
    json_array_mut(&mut duplicate_container, "/containers").push(container);
    assert_invalid_transport(duplicate_container);

    let mut duplicate_service = valid.clone();
    let service = duplicate_service.pointer("/services/0").unwrap().clone();
    json_array_mut(&mut duplicate_service, "/services").push(service);
    assert_invalid_transport(duplicate_service);

    let mut missing_member = valid.clone();
    *json_mut(&mut missing_member, "/services/0/member_ids/0") = json!(INCOMPLETE_CONTAINER_ID);
    assert_invalid_transport(missing_member);

    let mut duplicate_member = valid.clone();
    let member = duplicate_member
        .pointer("/services/0/member_ids/0")
        .unwrap()
        .clone();
    json_array_mut(&mut duplicate_member, "/services/0/member_ids").push(member);
    assert_invalid_transport(duplicate_member);

    let mut contradictory = valid.clone();
    *json_mut(&mut contradictory, "/services/0/identity") = json!("app/worker");
    assert_invalid_transport(contradictory);

    let mut unassigned = valid;
    *json_mut(&mut unassigned, "/services") = json!([]);
    assert_invalid_transport(unassigned);
}

#[test]
fn high_replica_transport_is_materially_smaller() {
    let frame = replicated_frame(30);
    let rich_size = serde_json::to_vec(&frame).unwrap().len();
    let transport = serde_json::to_vec(&RuntimeWatchTransportFrame::from_frame(&frame)).unwrap();
    let transport_value: Value = serde_json::from_slice(&transport).unwrap();

    assert_eq!(json_array(&transport_value, "/specs").len(), 1);
    assert_eq!(json_array(&transport_value, "/containers").len(), 30);
    assert!(
        transport.len() * 3 < rich_size,
        "{}/{rich_size}",
        transport.len()
    );
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

fn transport_value(frame: &RuntimeWatchFrame) -> Value {
    serde_json::to_value(RuntimeWatchTransportFrame::from_frame(frame)).unwrap()
}

fn json_array<'value>(value: &'value Value, pointer: &str) -> &'value [Value] {
    value.pointer(pointer).and_then(Value::as_array).unwrap()
}

fn json_array_mut<'value>(value: &'value mut Value, pointer: &str) -> &'value mut Vec<Value> {
    value
        .pointer_mut(pointer)
        .and_then(Value::as_array_mut)
        .unwrap()
}

fn json_mut<'value>(value: &'value mut Value, pointer: &str) -> &'value mut Value {
    value.pointer_mut(pointer).unwrap()
}

fn assert_invalid_transport(value: Value) {
    assert!(serde_json::from_value::<RuntimeWatchTransportFrame>(value).is_err());
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
            machine: Machine {
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
            membership: MembershipObservation::Up,
            storage: None,
            selected_endpoint: Some(SelectedEndpoint("203.0.113.10:51820".parse().unwrap())),
            rtt: Some(RttStatistics {
                median_ns: 1_500_000,
                population_stddev_ns: 250_000,
            }),
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

fn replicated_frame(replicas: usize) -> RuntimeWatchFrame {
    let mut frame = expected_frame();
    frame.machines.clear();
    frame.volumes.clear();
    frame.certificates.clear();
    frame.hosted_dns_hostname = None;
    frame.incomplete_ids = RuntimeWatchIncompleteIds::default();
    let mut template = frame.containers.pop().unwrap();
    template.resolved_spec.container.environment = (0..20)
        .map(|index| (format!("SETTING_{index}"), "x".repeat(80)))
        .collect();
    frame.containers = (0..replicas)
        .map(|index| {
            let mut container = template.clone();
            container.container_id = ContainerId::parse(format!("{index:064x}")).unwrap();
            container.display_name = format!("api-{index}");
            container.created_at_unix_nanos += i64::try_from(index).unwrap();
            container
        })
        .collect();
    frame.services = derive_services(frame.containers.iter().cloned());
    frame
}
