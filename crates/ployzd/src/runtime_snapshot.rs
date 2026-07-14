//! Canonical assembly of complete runtime snapshots from intent and testimony.

use ployz_core::ids::{MachineId, NamespaceId, NamespaceRevisionEntryId, ServiceId};
use ployz_core::machine_runtime::{
    MachineFactsSnapshot, ManagedContainerKind, ManagedContainerObservation,
};
use ployz_core::state::{
    ActiveMachineState, GatewayStatusObservation, IntentSnapshot, RouteBindingState,
    ServingTargetEntry,
};
use ployz_sdk_types::{
    MachineSnapshot, MachineTestimony, RouteCertLifecycle, RouteCertStatus,
    RuntimeDerivedCollectionSource, RuntimeDerivedCollectionStatus, RuntimeProjectionSource,
    RuntimeProjectionSources, RuntimePublicUrl, RuntimeServiceInstance, RuntimeServiceRelease,
    RuntimeServiceRevision, RuntimeSnapshot, ServiceContainerMembership, ServiceContainerTestimony,
    ServiceMachineTestimony, ServiceSnapshot, ServiceTestimony,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn from_sources(
    intent: IntentSnapshot,
    facts: &BTreeMap<MachineId, MachineFactsSnapshot>,
    gateway_statuses: &BTreeMap<MachineId, GatewayStatusObservation>,
    read_at_unix_seconds: u64,
) -> RuntimeSnapshot {
    let public_url = match intent.public_url_mode {
        ployz_core::cert::PublicUrlMode::Auto => RuntimePublicUrl::Auto {
            domain: match &intent.managed_lease {
                ployz_core::state::ManagedLeaseProjection::Unacquired => None,
                ployz_core::state::ManagedLeaseProjection::RecordOnly { lease }
                | ployz_core::state::ManagedLeaseProjection::Ready { lease, .. } => {
                    Some(lease.name.hostname_suffix())
                }
            },
        },
        ployz_core::cert::PublicUrlMode::BringYourOwn => RuntimePublicUrl::BringYourOwn,
        ployz_core::cert::PublicUrlMode::None => RuntimePublicUrl::None,
    };
    // Per-custom-hostname TLS status. A usable custom certificate is Verified; an
    // in-flight ACME challenge is Pending. Verified wins if both exist for a host.
    let mut certificate_status_by_host = BTreeMap::new();
    for cert in &intent.custom_certificates {
        let lifecycle = if cert.is_usable_at(read_at_unix_seconds) {
            RouteCertLifecycle::Verified
        } else {
            RouteCertLifecycle::Pending
        };
        certificate_status_by_host.insert(cert.hostname.clone(), lifecycle);
    }
    for challenge in &intent.acme_http01_challenges {
        certificate_status_by_host
            .entry(challenge.hostname().clone())
            .or_insert(RouteCertLifecycle::Pending);
    }
    let certificate_statuses = certificate_status_by_host
        .into_iter()
        .map(|(hostname, status)| RouteCertStatus { hostname, status })
        .collect::<Vec<_>>();
    let machine_ids = intent
        .active_machines
        .iter()
        .map(|machine| machine.machine_id.clone())
        .collect::<Vec<_>>();
    let machines = intent
        .active_machines
        .into_iter()
        .map(|active| machine_snapshot(active, facts, gateway_statuses))
        .collect::<Vec<_>>();
    let routes = intent.route_bindings;
    let services = intent
        .serving_target_entries
        .into_iter()
        .map(|active| service_snapshot(active, &routes, &machine_ids, facts))
        .collect::<Vec<_>>();
    let containers = machine_ids
        .iter()
        .filter_map(|machine_id| facts.get(machine_id))
        .flat_map(|facts| facts.containers().containers().iter().cloned())
        .collect::<Vec<_>>();
    let revisions = derive_revisions(&services, &containers);
    let releases = derive_releases(&services, &routes);
    let instances = derive_instances(&containers);
    let missing_link_count = missing_links(&services, &routes, &containers);

    RuntimeSnapshot {
        public_url,
        certificate_statuses,
        machines,
        services,
        routes,
        containers,
        projection_sources: RuntimeProjectionSources {
            intent: RuntimeProjectionSource {
                read_at_unix_seconds,
            },
            facts: RuntimeProjectionSource {
                read_at_unix_seconds,
            },
            revisions: derived_source(revisions.len(), missing_link_count),
            releases: derived_source(releases.len(), missing_link_count),
            instances: derived_source(instances.len(), missing_link_count),
        },
        revisions,
        releases,
        instances,
        updated_at_unix_seconds: read_at_unix_seconds,
    }
}

fn machine_snapshot(
    active: ActiveMachineState,
    facts: &BTreeMap<MachineId, MachineFactsSnapshot>,
    gateways: &BTreeMap<MachineId, GatewayStatusObservation>,
) -> MachineSnapshot {
    let testimony = match facts.get(&active.machine_id) {
        Some(facts) => MachineTestimony::Answered {
            endpoints: facts.endpoints().cloned(),
            gateway: gateways.get(&active.machine_id).cloned(),
            observed_container_count: facts.containers().containers().len(),
            disk_space: facts.disk_space(),
            last_observed_at_unix_seconds: facts.observed_at_unix_ms() / 1_000,
        },
        None => MachineTestimony::NoAnswer,
    };
    MachineSnapshot { active, testimony }
}

pub(crate) fn service_snapshot(
    active: ServingTargetEntry,
    routes: &[RouteBindingState],
    machine_ids: &[MachineId],
    facts: &BTreeMap<MachineId, MachineFactsSnapshot>,
) -> ServiceSnapshot {
    let route_bindings = routes
        .iter()
        .filter(|route| {
            route.namespace_id == active.namespace_id && route.service_id == active.service_id
        })
        .cloned()
        .collect::<Vec<_>>();
    let mut machines = Vec::with_capacity(machine_ids.len());
    let mut ready_container_count = 0;
    let mut observed_container_count = 0;
    for machine_id in machine_ids {
        let Some(facts) = facts.get(machine_id) else {
            machines.push(ServiceMachineTestimony::NoAnswer {
                machine_id: machine_id.clone(),
            });
            continue;
        };
        let containers = facts
            .containers()
            .containers()
            .iter()
            .filter(|container| {
                container.identity.kind == ManagedContainerKind::Service
                    && container.identity.namespace_id == active.namespace_id
                    && container.identity.service_id == active.service_id
            })
            .map(|container| ServiceContainerTestimony {
                membership: if container.identity.namespace_revision_entry_id
                    == active.namespace_revision_entry_id
                {
                    ServiceContainerMembership::ServingTargetMember
                } else {
                    ServiceContainerMembership::RetainedEvidence
                },
                observation: container.clone(),
            })
            .collect::<Vec<_>>();
        ready_container_count += containers
            .iter()
            .filter(|container| {
                container.membership == ServiceContainerMembership::ServingTargetMember
                    && container.observation.state.is_running()
            })
            .count();
        observed_container_count += containers.len();
        machines.push(ServiceMachineTestimony::Answered {
            machine_id: machine_id.clone(),
            containers,
        });
    }

    ServiceSnapshot {
        active,
        route_bindings,
        testimony: ServiceTestimony {
            ready_container_count,
            observed_container_count,
            machines,
        },
    }
}

pub(crate) fn derive_revisions(
    services: &[ServiceSnapshot],
    containers: &[ManagedContainerObservation],
) -> Vec<RuntimeServiceRevision> {
    let mut revisions = BTreeSet::new();
    for service in services {
        revisions.insert((
            service.active.namespace_id.clone(),
            service.active.service_id.clone(),
            service.active.namespace_revision_entry_id.clone(),
        ));
    }
    for container in containers {
        if container.identity.kind != ManagedContainerKind::Service {
            continue;
        }
        revisions.insert((
            container.identity.namespace_id.clone(),
            container.identity.service_id.clone(),
            container.identity.namespace_revision_entry_id.clone(),
        ));
    }
    revisions
        .into_iter()
        .map(
            |(namespace_id, service_id, namespace_revision_entry_id)| RuntimeServiceRevision {
                namespace_id,
                service_id,
                namespace_revision_entry_id,
            },
        )
        .collect()
}

pub(crate) fn derive_releases(
    services: &[ServiceSnapshot],
    routes: &[RouteBindingState],
) -> Vec<RuntimeServiceRelease> {
    let mut releases =
        BTreeMap::<(NamespaceId, ServiceId, NamespaceRevisionEntryId), Vec<_>>::new();
    let mut active_revisions = BTreeMap::new();
    for service in services {
        active_revisions.insert(
            (
                service.active.namespace_id.clone(),
                service.active.service_id.clone(),
            ),
            service.active.namespace_revision_entry_id.clone(),
        );
        releases
            .entry((
                service.active.namespace_id.clone(),
                service.active.service_id.clone(),
                service.active.namespace_revision_entry_id.clone(),
            ))
            .or_default();
    }
    for route in routes {
        let Some(entry_id) =
            active_revisions.get(&(route.namespace_id.clone(), route.service_id.clone()))
        else {
            continue;
        };
        releases
            .entry((
                route.namespace_id.clone(),
                route.service_id.clone(),
                entry_id.clone(),
            ))
            .or_default()
            .push(route.target.clone());
    }
    releases
        .into_iter()
        .map(
            |((namespace_id, service_id, namespace_revision_entry_id), routes)| {
                RuntimeServiceRelease {
                    namespace_id,
                    service_id,
                    namespace_revision_entry_id,
                    routes,
                }
            },
        )
        .collect()
}

pub(crate) fn derive_instances(
    containers: &[ManagedContainerObservation],
) -> Vec<RuntimeServiceInstance> {
    containers
        .iter()
        .filter(|container| container.identity.kind == ManagedContainerKind::Service)
        .map(|container| RuntimeServiceInstance {
            namespace_id: container.identity.namespace_id.clone(),
            machine_id: container.machine_id.clone(),
            container_id: container.container_id.clone(),
            service_id: container.identity.service_id.clone(),
            namespace_revision_entry_id: container.identity.namespace_revision_entry_id.clone(),
            operation_id: container.identity.operation_id.clone(),
            step_id: container.identity.step_id.clone(),
            state: container.state.clone(),
        })
        .collect()
}

pub(crate) fn missing_links(
    services: &[ServiceSnapshot],
    routes: &[RouteBindingState],
    containers: &[ManagedContainerObservation],
) -> usize {
    let serving = services
        .iter()
        .map(|service| {
            (
                service.active.namespace_id.clone(),
                service.active.service_id.clone(),
            )
        })
        .collect::<BTreeSet<_>>();
    routes
        .iter()
        .filter(|route| !serving.contains(&(route.namespace_id.clone(), route.service_id.clone())))
        .count()
        + containers
            .iter()
            .filter(|container| {
                !serving.contains(&(
                    container.identity.namespace_id.clone(),
                    container.identity.service_id.clone(),
                ))
            })
            .count()
}

fn derived_source(
    source_count: usize,
    missing_link_count: usize,
) -> RuntimeDerivedCollectionSource {
    RuntimeDerivedCollectionSource {
        status: if missing_link_count == 0 {
            RuntimeDerivedCollectionStatus::Complete
        } else {
            RuntimeDerivedCollectionStatus::Partial
        },
        source_count,
        missing_link_count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::cert::{
        AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
        ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, LeaseBearerToken,
        LeaseExpiresAt, LeaseIssuedAt, ManagedCertBundle, ManagedLeaseName, ManagedLeaseRecord,
        PublicUrlMode,
    };
    use ployz_core::dataplane::DataplaneProjection;
    use ployz_core::ids::CertId;
    use ployz_core::ops::RouteHostname;
    use ployz_core::state::{ControlPlaneEpoch, ManagedLeaseProjection};
    use ployz_sdk_types::{RouteCertLifecycle, RouteCertStatus, RuntimePublicUrl};

    #[test]
    fn public_url_reports_auto_without_inventing_a_domain_before_acquisition() {
        let snapshot = from_sources(
            intent(PublicUrlMode::Auto, ManagedLeaseProjection::Unacquired),
            &BTreeMap::new(),
            &BTreeMap::new(),
            1,
        );

        assert_eq!(snapshot.public_url, RuntimePublicUrl::Auto { domain: None });
    }

    #[test]
    fn public_url_reports_the_canonical_domain_for_each_acquired_auto_state() {
        let lease = lease_record();
        let record_only = from_sources(
            intent(
                PublicUrlMode::Auto,
                ManagedLeaseProjection::RecordOnly {
                    lease: lease.clone(),
                },
            ),
            &BTreeMap::new(),
            &BTreeMap::new(),
            1,
        );
        let ready = from_sources(
            intent(
                PublicUrlMode::Auto,
                ManagedLeaseProjection::Ready {
                    lease: lease.clone(),
                    bundle: bundle(&lease),
                },
            ),
            &BTreeMap::new(),
            &BTreeMap::new(),
            1,
        );

        let expected = RuntimePublicUrl::Auto {
            domain: Some("brisk-river-x7f3.up.ployz.app".to_owned()),
        };
        assert_eq!(record_only.public_url, expected.clone());
        assert_eq!(ready.public_url, expected);
    }

    #[test]
    fn public_url_reports_bring_your_own_without_managed_domain_data() {
        let snapshot = from_sources(
            intent(
                PublicUrlMode::BringYourOwn,
                ManagedLeaseProjection::RecordOnly {
                    lease: lease_record(),
                },
            ),
            &BTreeMap::new(),
            &BTreeMap::new(),
            1,
        );

        assert_eq!(snapshot.public_url, RuntimePublicUrl::BringYourOwn);
    }

    #[test]
    fn public_url_reports_none_without_managed_domain_data() {
        let snapshot = from_sources(
            intent(
                PublicUrlMode::None,
                ManagedLeaseProjection::RecordOnly {
                    lease: lease_record(),
                },
            ),
            &BTreeMap::new(),
            &BTreeMap::new(),
            1,
        );

        assert_eq!(snapshot.public_url, RuntimePublicUrl::None);
    }

    #[test]
    fn certificate_status_reports_a_usable_custom_certificate_as_verified() {
        let hostname = route_hostname("app.example.com");
        let mut source = intent(
            PublicUrlMode::BringYourOwn,
            ManagedLeaseProjection::Unacquired,
        );
        source.custom_certificates = vec![active_certificate(hostname.clone(), 1, 100)];

        let snapshot = from_sources(source, &BTreeMap::new(), &BTreeMap::new(), 50);

        assert_eq!(
            snapshot.certificate_statuses,
            vec![RouteCertStatus {
                hostname,
                status: RouteCertLifecycle::Verified,
            }]
        );
    }

    #[test]
    fn certificate_status_reports_an_acme_challenge_as_pending() {
        let hostname = route_hostname("app.example.com");
        let mut source = intent(
            PublicUrlMode::BringYourOwn,
            ManagedLeaseProjection::Unacquired,
        );
        source.acme_http01_challenges = vec![challenge(hostname.clone())];

        let snapshot = from_sources(source, &BTreeMap::new(), &BTreeMap::new(), 50);

        assert_eq!(
            snapshot.certificate_statuses,
            vec![RouteCertStatus {
                hostname,
                status: RouteCertLifecycle::Pending,
            }]
        );
    }

    #[test]
    fn verified_certificate_wins_when_the_same_hostname_has_a_pending_challenge() {
        let hostname = route_hostname("app.example.com");
        let mut source = intent(
            PublicUrlMode::BringYourOwn,
            ManagedLeaseProjection::Unacquired,
        );
        source.custom_certificates = vec![active_certificate(hostname.clone(), 1, 100)];
        source.acme_http01_challenges = vec![challenge(hostname.clone())];

        let snapshot = from_sources(source, &BTreeMap::new(), &BTreeMap::new(), 50);

        assert_eq!(
            snapshot.certificate_statuses,
            vec![RouteCertStatus {
                hostname,
                status: RouteCertLifecycle::Verified,
            }]
        );
    }

    fn intent(
        public_url_mode: PublicUrlMode,
        managed_lease: ManagedLeaseProjection,
    ) -> IntentSnapshot {
        IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_machine_id: MachineId::try_new("core").expect("machine id"),
            active_machines: Vec::new(),
            dataplane_projection: DataplaneProjection::try_new(Vec::new(), None)
                .expect("dataplane projection"),
            route_bindings: Vec::new(),
            serving_target_entries: Vec::new(),
            volume_pins: Vec::new(),
            nats_authorizations: Vec::new(),
            public_url_mode,
            managed_lease,
            custom_certificates: Vec::new(),
            acme_http01_challenges: Vec::new(),
        }
    }

    fn lease_record() -> ManagedLeaseRecord {
        ManagedLeaseRecord::try_new(
            ManagedLeaseName::try_new("brisk-river-x7f3").expect("lease name"),
            LeaseBearerToken::try_new("lease-token").expect("lease token"),
            LeaseIssuedAt::try_new(1).expect("issued at"),
            LeaseExpiresAt::try_new(100).expect("expires at"),
        )
        .expect("lease record")
    }

    fn bundle(lease: &ManagedLeaseRecord) -> ManagedCertBundle {
        ManagedCertBundle::try_new(
            lease.name.clone(),
            lease.name.wildcard_and_apex(),
            "certificate".to_owned(),
            "private-key".to_owned(),
            LeaseIssuedAt::try_new(1).expect("issued at"),
            LeaseExpiresAt::try_new(100).expect("expires at"),
        )
        .expect("certificate bundle")
    }

    fn route_hostname(value: &str) -> RouteHostname {
        RouteHostname::try_new(value).expect("route hostname")
    }

    fn active_certificate(
        hostname: RouteHostname,
        not_before: u64,
        not_after: u64,
    ) -> ActiveCertState {
        ActiveCertState {
            cert_id: CertId::try_new("cert_app_example_com").expect("certificate id"),
            hostname,
            bundle_ref: CertBundleRef::try_new(format!(
                "sha256:{}:/var/lib/ployz/certificates/cert_app_example_com.bundle",
                "a".repeat(64)
            ))
            .expect("bundle reference"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(not_before).expect("not before"),
                CertValidAt::try_new(not_after).expect("not after"),
            )
            .expect("validity window"),
        }
    }

    fn challenge(hostname: RouteHostname) -> AcmeHttp01Challenge {
        AcmeHttp01Challenge::try_new(
            hostname,
            AcmeChallengeToken::try_new("token").expect("challenge token"),
            AcmeChallengeValue::try_new("token.account-thumbprint").expect("challenge value"),
            AcmeChallengeTtlSeconds::try_new(900).expect("challenge ttl"),
        )
        .expect("challenge")
    }
}
