//! Assemble one complete Runtime Watch frame from replicated observations.

#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Watch stream (#318) is the only production caller"
    )
)]

use std::{collections::BTreeMap, time::SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use ployz_core::{
    CertificateAvailability, CertificateBackoff, CertificateFailureKind, CertificateObservation,
    ContainerId, ContainerObservation, DockerVolume, DockerVolumeId, IngressHost, IssuanceClock,
    IssuanceFailure, Machine, MachineId, MachineObservation, MembershipObservation, RttStatistics,
    RuntimeWatchFrame, RuntimeWatchIncompleteIds, SelectedEndpoint, derive_services,
};

use crate::{
    corrosion::{CertificateRow, Error, ReplicatedObservations, ReplicatedStore},
    hosted_dns::Reservation,
};

/// Replicated observations used to assemble one Runtime Watch frame.
///
/// Loaded from the store. Never from live Machine RPC or Docker fan-out.
pub(crate) struct RuntimeWatchSnapshot {
    pub machines: ReplicatedObservations<Machine, MachineId>,
    pub containers: ReplicatedObservations<ContainerObservation, ContainerId>,
    pub volumes: ReplicatedObservations<DockerVolume, DockerVolumeId>,
    pub certificates: ReplicatedObservations<(IngressHost, CertificateRow), IngressHost>,
    pub hosted_dns: Option<Reservation>,
}

/// Entry-local membership, selected endpoint, and RTT overlays.
///
/// Each field is independent. Missing telemetry is not a delete of the replicated Machine.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RuntimeWatchTelemetry {
    pub membership: BTreeMap<MachineId, MembershipObservation>,
    pub selected_endpoints: BTreeMap<MachineId, SelectedEndpoint>,
    pub rtt: BTreeMap<MachineId, RttStatistics>,
}

impl RuntimeWatchSnapshot {
    /// Read Machines, Containers, Docker Volumes, certificates, and hosted DNS from the store.
    ///
    /// # Errors
    ///
    /// Returns when a replicated row cannot be read or decoded.
    pub(crate) async fn from_store(store: &ReplicatedStore) -> Result<Self, Error> {
        Ok(Self {
            machines: store.machines().await?,
            containers: store.containers().await?,
            volumes: store.volumes().await?,
            certificates: store.certificate_rows().await?,
            hosted_dns: store.domain_reservation().await?,
        })
    }
}

/// Assemble one complete Runtime Watch frame.
///
/// Service observations are derived from replicated Containers. Certificate Material,
/// HTTP-01 challenge bytes, hosted DNS token/endpoint, Relay credentials, and Pairing
/// credentials are not copied onto the frame. Incomplete IDs are preserved as IDs.
///
/// When `telemetry` is `None`, replicated Machine rows stay and membership is unknown
/// except the entry Machine, which is Up, with no selected endpoint or RTT.
#[must_use]
pub(crate) fn assemble_runtime_watch_frame(
    snapshot: RuntimeWatchSnapshot,
    entry_id: &MachineId,
    telemetry: Option<&RuntimeWatchTelemetry>,
    observed_at: String,
) -> RuntimeWatchFrame {
    let machines = snapshot
        .machines
        .observations
        .into_iter()
        .map(|machine| observe_machine(machine, entry_id, telemetry))
        .collect();
    let services = derive_services(snapshot.containers.observations.iter().cloned());
    let certificates = snapshot
        .certificates
        .observations
        .into_iter()
        .map(|(hostname, row)| redact_certificate(hostname, &row))
        .collect();
    RuntimeWatchFrame {
        machines,
        containers: snapshot.containers.observations,
        services,
        volumes: snapshot.volumes.observations,
        certificates,
        hosted_dns_hostname: snapshot.hosted_dns.map(|reservation| reservation.name),
        incomplete_ids: RuntimeWatchIncompleteIds {
            machines: snapshot.machines.incomplete_ids,
            containers: snapshot.containers.incomplete_ids,
            volumes: snapshot.volumes.incomplete_ids,
            certificates: snapshot.certificates.incomplete_ids,
        },
        observed_at,
    }
}

fn observe_machine(
    machine: Machine,
    entry_id: &MachineId,
    telemetry: Option<&RuntimeWatchTelemetry>,
) -> MachineObservation {
    let is_entry = &machine.id == entry_id;
    let Some(telemetry) = telemetry else {
        return MachineObservation {
            membership: if is_entry {
                MembershipObservation::Up
            } else {
                MembershipObservation::Unknown
            },
            selected_endpoint: None,
            rtt: None,
            machine,
        };
    };
    MachineObservation {
        membership: if is_entry {
            MembershipObservation::Up
        } else {
            telemetry
                .membership
                .get(&machine.id)
                .cloned()
                .unwrap_or(MembershipObservation::Unknown)
        },
        selected_endpoint: telemetry.selected_endpoints.get(&machine.id).copied(),
        rtt: telemetry.rtt.get(&machine.id).cloned(),
        machine,
    }
}

fn redact_certificate(hostname: IngressHost, row: &CertificateRow) -> CertificateObservation {
    let backoff = row.clock().map(certificate_backoff);
    let status = if row.material().is_some() {
        CertificateAvailability::Available
    } else if row.challenge().is_some() {
        CertificateAvailability::Pending
    } else if backoff.is_some() || row.last_error().is_some() {
        CertificateAvailability::Failure
    } else {
        CertificateAvailability::Unknown
    };
    CertificateObservation {
        hostname,
        status,
        last_error: row.last_error().map(str::to_owned),
        backoff,
    }
}

fn certificate_backoff(clock: IssuanceClock) -> CertificateBackoff {
    CertificateBackoff {
        failure_kind: match clock.last_failure() {
            IssuanceFailure::DoesNotResolve => CertificateFailureKind::DoesNotResolve,
            IssuanceFailure::ResolvesElsewhere => CertificateFailureKind::ResolvesElsewhere,
            IssuanceFailure::Authority => CertificateFailureKind::Authority,
        },
        next_attempt_at: rfc3339(clock.next_attempt_at()),
        failures: clock.failures(),
    }
}

fn rfc3339(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        time::{Duration, SystemTime},
    };

    use ployz_core::{
        AdvertisedEndpoint, CertificateAvailability, CertificateBackoff, CertificateFailureKind,
        CertificateObservation, ContainerId, ContainerKind, ContainerObservation,
        ContainerRuntimeObservation, DockerVolume, DockerVolumeId, DockerVolumeName,
        HealthObservation, HookContainer, IngressHost, IssuanceClock, IssuanceFailure, Machine,
        MachineId, MachineName, MachineObservation, MachineRuntime, ManagementAddress,
        MembershipObservation, OpaquePayload, ResolvedServiceSpec, RttStatistics, SelectedEndpoint,
        ServiceContainer, ServiceId, ServiceName, ServiceObservation, WireGuardPublicKey,
    };
    use serde_json::{Value, json};

    use super::{RuntimeWatchSnapshot, RuntimeWatchTelemetry, assemble_runtime_watch_frame};
    use crate::corrosion::{
        CertificateChallenge, CertificateMaterial, CertificateRow, ReplicatedObservations,
    };
    use crate::hosted_dns::Reservation;

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
        let mut telemetry = RuntimeWatchTelemetry::default();
        telemetry
            .membership
            .insert(peer.id, MembershipObservation::Suspect);
        telemetry.selected_endpoints.insert(entry.id, endpoint);
        telemetry.rtt.insert(entry.id, rtt.clone());

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
        let pending = CertificateRow::from_parts(None, None).with_challenge(
            CertificateChallenge::new(CHALLENGE_TOKEN, CHALLENGE_RESPONSE).unwrap(),
        );
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
}
