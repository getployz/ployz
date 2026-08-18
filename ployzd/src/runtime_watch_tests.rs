//! Tests for complete Runtime Watch frame assembly and sampling.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use futures_util::StreamExt;
use ployz_core::{
    AdvertisedEndpoint, CertificateAvailability, CertificateBackoff, CertificateFailureKind,
    CertificateObservation, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, DockerVolume, DockerVolumeId, DockerVolumeName, HealthObservation,
    HookContainer, IngressHost, IssuanceClock, IssuanceFailure, Machine, MachineId, MachineName,
    MachineObservation, MachineRuntime, ManagementAddress, MembershipObservation, OpaquePayload,
    ResolvedServiceSpec, RttObservation, RttStatistics, SelectedEndpoint, ServiceContainer,
    ServiceId, ServiceName, ServiceObservation, WireGuardPublicKey,
};
use serde_json::{Value, json};

use super::{
    LatestSample, RuntimeWatchSnapshot, RuntimeWatchTelemetry, assemble_runtime_watch_frame,
    serve_runtime_watch,
};
use crate::corrosion::{
    CertificateChallenge, CertificateMaterial, CertificateRow, Error, ReplicatedObservations,
};
use crate::hosted_dns::Reservation;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;

const ENTRY_ID: &str = "0123456789abcdef0123456789abcdef";
const PEER_ID: &str = "fedcba9876543210fedcba9876543210";
const SERVICE_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTAINER_ID: &str = "1111111111111111111111111111111111111111111111111111111111111111";
const HOOK_ID: &str = "3333333333333333333333333333333333333333333333333333333333333333";
const INCOMPLETE_CONTAINER_ID: &str =
    "2222222222222222222222222222222222222222222222222222222222222222";
const OBSERVED_AT: &str = "2024-01-01T00:00:00Z";
const CERT: &str = "-----BEGIN CERTIFICATE-----\nSECRETCERT\n-----END CERTIFICATE-----";
const KEY: &str = "-----BEGIN PRIVATE KEY-----\nSECRETKEY\n-----END PRIVATE KEY-----";
const CHALLENGE_TOKEN: &str = "http-01-token-secret";
const CHALLENGE_RESPONSE: &str = "http-01-response-secret";
const DNS_TOKEN: &str = "dns-renewal-token-secret";
const DNS_ENDPOINT: &str = "https://dns.example.invalid/v1";
const PAIRING: &str = "pairing-credential-secret";
const DIAL: &str = "dial-credential-secret";

#[test]
fn assembled_frame_keeps_replicated_rows_and_derives_services() {
    let entry = machine("edge", ENTRY_ID, 1);
    let peer = machine("peer", PEER_ID, 2);
    let service = container(CONTAINER_ID, "api", ContainerKind::ServiceContainer);
    let hook = container(HOOK_ID, "api", ContainerKind::PreDeployHook);
    let volume = volume_on(ENTRY_ID, "data");
    let endpoint = SelectedEndpoint("203.0.113.10:51820".parse().unwrap());
    let rtt = RttStatistics {
        median_ns: 1_500_000,
        population_stddev_ns: 250_000,
    };
    let telemetry = RuntimeWatchTelemetry {
        states: BTreeMap::from([(peer.management_address, MembershipObservation::Suspect)]),
        selected_endpoints: BTreeMap::from([(entry.id, endpoint)]),
        rtts: vec![rtt_on(&entry, rtt.clone())],
    };

    let frame = assemble_runtime_watch_frame(
        RuntimeWatchSnapshot {
            machines: observations(vec![entry.clone(), peer.clone()]),
            containers: observations(vec![service.clone(), hook.clone()]),
            volumes: observations(vec![volume.clone()]),
            certificates: ReplicatedObservations {
                observations: vec![(
                    IngressHost::parse("ok.example.com").unwrap(),
                    CertificateRow::issued(CertificateMaterial::new(CERT, KEY).unwrap()),
                )],
                incomplete_ids: Vec::new(),
            },
            hosted_dns: Some(reservation()),
        },
        &entry.id,
        Some(&telemetry),
        OBSERVED_AT.into(),
    );

    assert_eq!(
        frame.machines,
        vec![
            MachineObservation {
                machine: entry,
                membership: MembershipObservation::Up,
                selected_endpoint: Some(endpoint),
                rtt: Some(rtt),
            },
            MachineObservation {
                machine: peer,
                membership: MembershipObservation::Suspect,
                selected_endpoint: None,
                rtt: None,
            },
        ]
    );
    assert_eq!(frame.containers, vec![service.clone(), hook.clone()]);
    assert_eq!(
        frame.services,
        vec![ServiceObservation {
            service_id: ServiceId::parse(SERVICE_ID).unwrap(),
            containers: vec![ServiceContainer::try_from(service).unwrap()],
            hook_containers: vec![HookContainer::try_from(hook).unwrap()],
        }]
    );
    assert_eq!(frame.volumes, vec![volume]);
    assert_eq!(
        frame.certificates,
        vec![CertificateObservation {
            hostname: IngressHost::parse("ok.example.com").unwrap(),
            status: CertificateAvailability::Available,
            last_error: None,
            backoff: None,
        }]
    );
    assert_eq!(
        frame.hosted_dns_hostname.as_deref(),
        Some("cluster.example.ts.net")
    );
    assert_eq!(frame.observed_at, OBSERVED_AT);
}

#[test]
fn incomplete_ids_are_preserved_and_are_not_deletes() {
    let entry = machine("edge", ENTRY_ID, 1);
    let kept = container(CONTAINER_ID, "api", ContainerKind::ServiceContainer);
    let kept_volume = volume_on(ENTRY_ID, "data");
    let incomplete_machine = MachineId::parse(PEER_ID).unwrap();
    let incomplete_container = ContainerId::parse(INCOMPLETE_CONTAINER_ID).unwrap();
    let incomplete_volume = DockerVolumeId {
        machine_id: incomplete_machine,
        name: DockerVolumeName::parse("scratch").unwrap(),
    };
    let incomplete_cert = IngressHost::parse("pending.example.com").unwrap();

    let frame = assemble_runtime_watch_frame(
        RuntimeWatchSnapshot {
            machines: ReplicatedObservations {
                observations: vec![entry.clone()],
                incomplete_ids: vec![incomplete_machine],
            },
            containers: ReplicatedObservations {
                observations: vec![kept.clone()],
                incomplete_ids: vec![incomplete_container],
            },
            volumes: ReplicatedObservations {
                observations: vec![kept_volume.clone()],
                incomplete_ids: vec![incomplete_volume.clone()],
            },
            certificates: ReplicatedObservations {
                observations: vec![(
                    IngressHost::parse("ok.example.com").unwrap(),
                    CertificateRow::issued(CertificateMaterial::new(CERT, KEY).unwrap()),
                )],
                incomplete_ids: vec![incomplete_cert.clone()],
            },
            hosted_dns: None,
        },
        &entry.id,
        None,
        OBSERVED_AT.into(),
    );

    assert_eq!(
        frame.machines,
        vec![MachineObservation {
            machine: entry,
            membership: MembershipObservation::Up,
            selected_endpoint: None,
            rtt: None,
        }]
    );
    assert_eq!(frame.containers, vec![kept]);
    assert_eq!(frame.volumes, vec![kept_volume]);
    assert_eq!(frame.certificates.len(), 1);
    assert_eq!(frame.incomplete_ids.machines, vec![incomplete_machine]);
    assert_eq!(frame.incomplete_ids.containers, vec![incomplete_container]);
    assert_eq!(frame.incomplete_ids.volumes, vec![incomplete_volume]);
    assert_eq!(frame.incomplete_ids.certificates, vec![incomplete_cert]);
    assert!(
        !frame
            .containers
            .iter()
            .any(|container| container.container_id.as_str() == INCOMPLETE_CONTAINER_ID)
    );
}

#[test]
fn serialized_frame_redacts_certificate_material_and_dns_credentials() {
    let entry = machine("edge", ENTRY_ID, 1);
    let at = SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_067_200);
    let clock = IssuanceClock::new(2, at, IssuanceFailure::DoesNotResolve);
    let pending = CertificateRow::from_parts(None, None)
        .with_challenge(CertificateChallenge::new(CHALLENGE_TOKEN, CHALLENGE_RESPONSE).unwrap());
    let failed = CertificateRow::from_parts(None, None).with_backoff(
        "Ingress Hostname app.example.com does not resolve; it should resolve to 192.0.2.1.",
        clock,
    );
    let frame = assemble_runtime_watch_frame(
        RuntimeWatchSnapshot {
            machines: observations(vec![entry.clone()]),
            containers: observations(Vec::new()),
            volumes: observations(Vec::new()),
            certificates: ReplicatedObservations {
                observations: vec![
                    (
                        IngressHost::parse("ok.example.com").unwrap(),
                        CertificateRow::issued(CertificateMaterial::new(CERT, KEY).unwrap()),
                    ),
                    (IngressHost::parse("new.example.com").unwrap(), pending),
                    (IngressHost::parse("app.example.com").unwrap(), failed),
                    (
                        IngressHost::parse("maybe.example.com").unwrap(),
                        CertificateRow::default(),
                    ),
                ],
                incomplete_ids: Vec::new(),
            },
            hosted_dns: Some(reservation()),
        },
        &entry.id,
        None,
        OBSERVED_AT.into(),
    );

    assert_eq!(
        frame.certificates,
        vec![
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
                    next_attempt_at: "2024-01-01T00:00:00Z".into(),
                    failures: 2,
                }),
            },
            CertificateObservation {
                hostname: IngressHost::parse("maybe.example.com").unwrap(),
                status: CertificateAvailability::Unknown,
                last_error: None,
                backoff: None,
            },
        ]
    );

    let encoded = OpaquePayload::from_json(&frame).unwrap();
    let round_trip: Value = encoded.decode_json().unwrap();
    assert_no_secret_material(&round_trip.to_string());
    assert_eq!(
        round_trip.get("hosted_dns_hostname"),
        Some(&json!("cluster.example.ts.net"))
    );
    assert!(round_trip.get("endpoint").is_none());
    assert!(round_trip.get("token").is_none());
    let certificates = round_trip
        .get("certificates")
        .and_then(Value::as_array)
        .expect("certificates");
    let available = certificates.first().expect("available certificate");
    let pending = certificates.get(1).expect("pending certificate");
    assert!(available.get("certificate").is_none());
    assert!(available.get("private_key").is_none());
    assert!(pending.get("challenge_token").is_none());
    assert!(pending.get("challenge_response").is_none());
}

#[test]
fn unavailable_telemetry_keeps_replicated_machines_with_entry_up() {
    let entry = machine("edge", ENTRY_ID, 1);
    let peer = machine("peer", PEER_ID, 2);
    let frame = assemble_runtime_watch_frame(
        RuntimeWatchSnapshot {
            machines: observations(vec![entry.clone(), peer.clone()]),
            containers: observations(Vec::new()),
            volumes: observations(Vec::new()),
            certificates: ReplicatedObservations {
                observations: Vec::new(),
                incomplete_ids: Vec::new(),
            },
            hosted_dns: None,
        },
        &entry.id,
        None,
        OBSERVED_AT.into(),
    );

    assert_eq!(
        frame.machines,
        vec![
            MachineObservation {
                machine: entry,
                membership: MembershipObservation::Up,
                selected_endpoint: None,
                rtt: None,
            },
            MachineObservation {
                machine: peer,
                membership: MembershipObservation::Unknown,
                selected_endpoint: None,
                rtt: None,
            },
        ]
    );
    assert_eq!(frame.hosted_dns_hostname, None);
}

fn observations<T, Id>(observations: Vec<T>) -> ReplicatedObservations<T, Id> {
    ReplicatedObservations {
        observations,
        incomplete_ids: Vec::new(),
    }
}

fn reservation() -> Reservation {
    Reservation {
        endpoint: DNS_ENDPOINT.into(),
        name: "cluster.example.ts.net".into(),
        token: DNS_TOKEN.into(),
    }
}

fn machine(name: &str, id: &str, seed: u8) -> Machine {
    Machine {
        id: MachineId::parse(id).unwrap(),
        name: MachineName::parse(name).unwrap(),
        subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
        management_address: ManagementAddress(format!("fdcc::{seed}").parse().unwrap()),
        public_key: WireGuardPublicKey([seed; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("203.0.113.{seed}:51820").parse().unwrap(),
        )],
        runtime: MachineRuntime::default(),
    }
}

fn rtt_on(machine: &Machine, statistics: RttStatistics) -> RttObservation {
    RttObservation {
        peer_id: machine.name.as_str().into(),
        address: format!("[{}]:51001", machine.management_address.0)
            .parse()
            .unwrap(),
        machine: None,
        statistics,
    }
}

fn peer_sample(
    peer: &Machine,
    membership: MembershipObservation,
    rtt: Option<RttStatistics>,
) -> RuntimeWatchTelemetry {
    RuntimeWatchTelemetry {
        states: BTreeMap::from([(peer.management_address, membership)]),
        selected_endpoints: BTreeMap::new(),
        rtts: rtt
            .map(|statistics| vec![rtt_on(peer, statistics)])
            .unwrap_or_default(),
    }
}

fn volume_on(machine_id: &str, name: &str) -> DockerVolume {
    DockerVolume {
        id: DockerVolumeId {
            machine_id: MachineId::parse(machine_id).unwrap(),
            name: DockerVolumeName::parse(name).unwrap(),
        },
        driver: "local".into(),
        options: BTreeMap::from([("type".into(), "none".into())]),
        labels: BTreeMap::from([("purpose".into(), "database".into())]),
    }
}

fn container(id: &str, service_name: &str, kind: ContainerKind) -> ContainerObservation {
    let service_id = ServiceId::parse(SERVICE_ID).unwrap();
    let service_name = ServiceName::parse(service_name).unwrap();
    let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": service_id,
        "name": service_name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "api:1", "pull_policy": "missing" }
    }))
    .unwrap();
    ContainerObservation {
        container_id: ContainerId::parse(id).unwrap(),
        display_name: "api-1".into(),
        created_at_unix_nanos: 1_700_000_000_000_000_000,
        machine_id: MachineId::parse(ENTRY_ID).unwrap(),
        service_id,
        service_name,
        kind,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec,
        address: None,
        labels: Default::default(),
    }
}

fn assert_no_secret_material(text: &str) {
    for forbidden in [
        "BEGIN CERTIFICATE",
        "BEGIN PRIVATE KEY",
        "SECRETCERT",
        "SECRETKEY",
        CHALLENGE_TOKEN,
        CHALLENGE_RESPONSE,
        DNS_TOKEN,
        DNS_ENDPOINT,
        PAIRING,
        DIAL,
        "private_key",
        "challenge_token",
        "challenge_response",
        "renewal_token",
    ] {
        assert!(
            !text.contains(forbidden),
            "{forbidden} must not appear on the Watch frame"
        );
    }
}

#[tokio::test]
async fn watch_stream_yields_the_first_complete_frame_immediately() {
    let entry = machine("edge", ENTRY_ID, 1);
    let volume = volume_on(ENTRY_ID, "data");
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], vec![volume.clone()]));
    let (_wake, changes) = mpsc::channel(1);
    let mut stream = serve_fixture(entry.id, &fixture, changes);

    let frame = next_frame(&mut stream).await;
    assert_eq!(frame.volumes, vec![volume]);
    let machine = frame.machines.first().expect("entry Machine");
    assert_eq!(machine.membership, MembershipObservation::Up);
    assert_eq!(machine.rtt, None);
    assert_eq!(frame.observed_at, OBSERVED_AT);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .is_err(),
        "first yield must not wait for a timer or a store change"
    );
}

#[tokio::test]
async fn store_notification_yields_when_the_assembled_observation_changes() {
    let entry = machine("edge", ENTRY_ID, 1);
    let first = volume_on(ENTRY_ID, "data");
    let second = volume_on(ENTRY_ID, "logs");
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], vec![first.clone()]));
    let (wake, changes) = mpsc::channel(1);
    let mut stream = serve_fixture(entry.id, &fixture, changes);

    let frame = next_frame(&mut stream).await;
    assert_eq!(frame.volumes, vec![first.clone()]);

    fixture.set(snapshot(
        vec![entry.clone()],
        vec![first.clone(), second.clone()],
    ));
    wake.send(Ok(())).await.unwrap();

    let frame = next_frame(&mut stream).await;
    assert_eq!(frame.volumes, vec![first, second]);
}

#[tokio::test]
async fn unchanged_assembled_observation_does_not_yield() {
    let entry = machine("edge", ENTRY_ID, 1);
    let volume = volume_on(ENTRY_ID, "data");
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], vec![volume]));
    let (wake, changes) = mpsc::channel(1);
    let mut stream = serve_fixture(entry.id, &fixture, changes);

    let _first = next_frame(&mut stream).await;
    wake.send(Ok(())).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .is_err(),
        "a store wake whose reassembled observation is unchanged must not yield"
    );
}

#[tokio::test]
async fn store_failure_ends_the_stream() {
    let entry = machine("edge", ENTRY_ID, 1);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], Vec::new()));
    let (wake, changes) = mpsc::channel(1);
    let mut stream = serve_fixture(entry.id, &fixture, changes);

    let _first = next_frame(&mut stream).await;
    fixture.fail("store closed");
    wake.send(Ok(())).await.unwrap();

    let error = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("failure must end the stream")
        .expect("stream item")
        .expect_err("store failure is a stream error");
    assert_eq!(error.code(), tonic::Code::Unavailable);
    assert!(
        tokio::time::timeout(Duration::from_secs(1), stream.next())
            .await
            .expect("ended stream")
            .is_none()
    );
}

#[tokio::test]
async fn dropping_the_stream_cancels_watch() {
    let entry = machine("edge", ENTRY_ID, 1);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], Vec::new()));
    let (wake, changes) = mpsc::channel(1);
    let mut stream = serve_fixture(entry.id, &fixture, changes);

    let _first = next_frame(&mut stream).await;
    drop(stream);
    tokio::time::timeout(Duration::from_secs(1), wake.closed())
        .await
        .expect("dropping the Watch stream must cancel the store subscription");
}

#[tokio::test]
async fn reconnect_yields_a_fresh_complete_frame() {
    let entry = machine("edge", ENTRY_ID, 1);
    let first = volume_on(ENTRY_ID, "data");
    let replacement = volume_on(ENTRY_ID, "scratch");
    let fixture = WatchFixture::new(snapshot(vec![entry.clone()], vec![first.clone()]));
    let (_wake, changes) = mpsc::channel(1);
    let mut stream = serve_fixture(entry.id, &fixture, changes);

    let first_frame = next_frame(&mut stream).await;
    assert_eq!(first_frame.volumes, vec![first]);
    drop(stream);

    fixture.set(snapshot(vec![entry.clone()], vec![replacement.clone()]));
    let (_wake, changes) = mpsc::channel(1);
    let mut stream = serve_fixture(entry.id, &fixture, changes);
    let reconnect = next_frame(&mut stream).await;
    assert_eq!(reconnect.volumes, vec![replacement]);
    assert_eq!(reconnect.machines.len(), 1);
}

#[tokio::test]
async fn watch_stream_reports_entry_up_and_sampled_rtt() {
    let entry = machine("edge", ENTRY_ID, 1);
    let peer = machine("peer", PEER_ID, 2);
    let rtt = RttStatistics {
        median_ns: 1_500_000,
        population_stddev_ns: 250_000,
    };
    let telemetry = peer_sample(&peer, MembershipObservation::Up, Some(rtt.clone()));
    let fixture = WatchFixture::new(snapshot(vec![entry.clone(), peer.clone()], Vec::new()));
    fixture.set_sample(Some(telemetry), OBSERVED_AT);
    let (_wake, changes) = mpsc::channel(1);
    let mut stream = serve_fixture(entry.id, &fixture, changes);

    let frame = next_frame(&mut stream).await;
    let entry_row = frame.machines.first().expect("entry Machine");
    let peer_row = frame.machines.get(1).expect("peer Machine");
    assert_eq!(entry_row.membership, MembershipObservation::Up);
    assert_eq!(entry_row.rtt, None);
    assert_eq!(peer_row.membership, MembershipObservation::Up);
    assert_eq!(peer_row.rtt, Some(rtt));
    assert_eq!(frame.observed_at, OBSERVED_AT);
}

#[tokio::test]
async fn semantic_telemetry_change_yields_a_new_complete_frame() {
    let entry = machine("edge", ENTRY_ID, 1);
    let peer = machine("peer", PEER_ID, 2);
    let first = peer_sample(&peer, MembershipObservation::Up, None);
    let second = peer_sample(&peer, MembershipObservation::Suspect, None);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone(), peer.clone()], Vec::new()));
    fixture.set_sample(Some(first), OBSERVED_AT);
    let (_wake, changes) = mpsc::channel(1);
    let (ticks, tick_rx) = mpsc::channel(1);
    let mut stream = serve_sampled(entry.id, &fixture, changes, tick_rx);

    let frame = next_frame(&mut stream).await;
    assert_eq!(
        frame.machines.get(1).expect("peer Machine").membership,
        MembershipObservation::Up
    );

    fixture.set_sample(Some(second), "2024-01-01T00:00:01Z");
    ticks.send(()).await.unwrap();

    let frame = next_frame(&mut stream).await;
    assert_eq!(
        frame.machines.get(1).expect("peer Machine").membership,
        MembershipObservation::Suspect
    );
    assert_eq!(frame.observed_at, "2024-01-01T00:00:01Z");
}

#[tokio::test]
async fn unchanged_telemetry_sample_does_not_yield_when_observed_at_advances() {
    let entry = machine("edge", ENTRY_ID, 1);
    let peer = machine("peer", PEER_ID, 2);
    let telemetry = peer_sample(&peer, MembershipObservation::Up, None);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone(), peer.clone()], Vec::new()));
    fixture.set_sample(Some(telemetry.clone()), OBSERVED_AT);
    let (_wake, changes) = mpsc::channel(1);
    let (ticks, tick_rx) = mpsc::channel(1);
    let mut stream = serve_sampled(entry.id, &fixture, changes, tick_rx);

    let first = next_frame(&mut stream).await;
    assert_eq!(first.observed_at, OBSERVED_AT);

    fixture.set_sample(Some(telemetry), "2024-01-01T00:00:01Z");
    ticks.send(()).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .is_err(),
        "an unchanged membership/RTT sample must not yield because time or observed_at advanced"
    );
}

#[tokio::test]
async fn unavailable_sample_after_telemetry_keeps_replicated_rows() {
    let entry = machine("edge", ENTRY_ID, 1);
    let peer = machine("peer", PEER_ID, 2);
    let telemetry = peer_sample(&peer, MembershipObservation::Up, None);
    let fixture = WatchFixture::new(snapshot(vec![entry.clone(), peer.clone()], Vec::new()));
    fixture.set_sample(Some(telemetry), OBSERVED_AT);
    let (_wake, changes) = mpsc::channel(1);
    let (ticks, tick_rx) = mpsc::channel(1);
    let mut stream = serve_sampled(entry.id, &fixture, changes, tick_rx);

    let frame = next_frame(&mut stream).await;
    assert_eq!(
        frame.machines.get(1).expect("peer Machine").membership,
        MembershipObservation::Up
    );

    fixture.set_sample(None, "2024-01-01T00:00:01Z");
    ticks.send(()).await.unwrap();
    let frame = next_frame(&mut stream).await;
    assert_eq!(
        frame.machines.get(1).expect("peer Machine").membership,
        MembershipObservation::Unknown
    );
    assert_eq!(frame.machines.get(1).expect("peer Machine").rtt, None);
}

#[derive(Clone)]
struct WatchFixture {
    snapshot: Arc<Mutex<Result<RuntimeWatchSnapshot, String>>>,
    sample: Arc<Mutex<(Option<RuntimeWatchTelemetry>, String)>>,
}

impl WatchFixture {
    fn new(snapshot: RuntimeWatchSnapshot) -> Self {
        Self {
            snapshot: Arc::new(Mutex::new(Ok(snapshot))),
            sample: Arc::new(Mutex::new((None, OBSERVED_AT.into()))),
        }
    }

    fn set(&self, snapshot: RuntimeWatchSnapshot) {
        *self.snapshot.lock().unwrap() = Ok(snapshot);
    }

    fn set_sample(&self, telemetry: Option<RuntimeWatchTelemetry>, observed_at: &str) {
        *self.sample.lock().unwrap() = (telemetry, observed_at.into());
    }

    fn fail(&self, message: &str) {
        *self.snapshot.lock().unwrap() = Err(message.into());
    }

    async fn load(&self) -> Result<RuntimeWatchSnapshot, Error> {
        match &*self.snapshot.lock().unwrap() {
            Ok(snapshot) => Ok(snapshot.clone()),
            Err(message) => Err(Error::Protocol(message.clone())),
        }
    }

    fn sample(&self) -> LatestSample {
        let (telemetry, observed_at) = self.sample.lock().unwrap().clone();
        LatestSample {
            telemetry,
            observed_at,
        }
    }
}

fn serve_fixture(
    entry_id: MachineId,
    fixture: &WatchFixture,
    changes: mpsc::Receiver<Result<(), Error>>,
) -> crate::logs::RpcStream {
    serve_sampled(entry_id, fixture, changes, mpsc::channel(1).1)
}

fn serve_sampled(
    entry_id: MachineId,
    fixture: &WatchFixture,
    changes: mpsc::Receiver<Result<(), Error>>,
    ticks: mpsc::Receiver<()>,
) -> crate::logs::RpcStream {
    let load_fixture = fixture.clone();
    let sample_fixture = fixture.clone();
    serve_runtime_watch(
        entry_id,
        move || {
            let fixture = load_fixture.clone();
            async move { fixture.load().await }
        },
        move || {
            let fixture = sample_fixture.clone();
            async move { fixture.sample() }
        },
        ReceiverStream::new(changes),
        ReceiverStream::new(ticks),
    )
}

async fn next_frame(stream: &mut crate::logs::RpcStream) -> ployz_core::RuntimeWatchFrame {
    let payload = tokio::time::timeout(Duration::from_secs(1), stream.next())
        .await
        .expect("Watch frame")
        .expect("open stream")
        .expect("Watch status");
    payload.decode_json().unwrap()
}

fn snapshot(machines: Vec<Machine>, volumes: Vec<DockerVolume>) -> RuntimeWatchSnapshot {
    RuntimeWatchSnapshot {
        machines: observations(machines),
        containers: observations(Vec::new()),
        volumes: observations(volumes),
        certificates: ReplicatedObservations {
            observations: Vec::new(),
            incomplete_ids: Vec::new(),
        },
        hosted_dns: None,
    }
}
