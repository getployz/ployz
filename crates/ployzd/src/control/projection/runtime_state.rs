//! Canonical assembly of complete runtime snapshots from intent and testimony.

use ployz_core::ids::{MachineId, NamespaceId, NamespaceRevisionEntryId, ServiceId};
use ployz_core::ingress::{
    CertificateOwner, IngressEndpointProjection, PloyzDnsTargetIntent, RouteBindingOrigin,
};
use ployz_core::intent::ActiveMachineState;
use ployz_core::intent::IntentSnapshot;
use ployz_core::intent::RouteBindingState;
use ployz_core::intent::ServingTargetEntry;
use ployz_core::machine::GatewayStatusObservation;
use ployz_core::machine::runtime::{
    MachineFactsSnapshot, ManagedContainerKind, ManagedContainerObservation,
};
use ployz_core::machine::{MachineStorageTestimony, derive_stranded_volume_alarms};

use ployz_sdk_types::{
    MachineSnapshot, MachineTestimony, RouteTlsAvailability, RouteTlsStatus,
    RuntimeDerivedCollectionSource, RuntimeDerivedCollectionStatus, RuntimePloyzDnsTarget,
    RuntimePloyzDnsTargetAllocation, RuntimePloyzDnsTargetPublication, RuntimeProjectionSource,
    RuntimeProjectionSources, RuntimeServiceInstance, RuntimeServiceRelease,
    RuntimeServiceRevision, RuntimeSnapshot, ServiceContainerMembership, ServiceContainerTestimony,
    ServiceMachineTestimony, ServiceSnapshot, ServiceTestimony,
};
use std::collections::{BTreeMap, BTreeSet};

use crate::control::intent::ingress_intent::{
    IngressProjectionStore, ManagedDnsCheckpoint, PloyzDnsTargetAllocation, PloyzDnsTargetStore,
};
use crate::control::store::CoreStore;

#[derive(Debug, Clone)]
pub(crate) struct RuntimeIngressSources {
    pub(crate) ployz_dns_target_allocation: Option<PloyzDnsTargetAllocation>,
    pub(crate) ployz_dns_target_checkpoint: Option<ManagedDnsCheckpoint>,
    pub(crate) ingress_endpoint_projection: IngressEndpointProjection,
}

pub(crate) async fn load_ingress_sources(
    store: &CoreStore,
) -> Result<RuntimeIngressSources, String> {
    let target = PloyzDnsTargetStore::new(store.clone());
    let (allocation, checkpoint) = target
        .load_state()
        .await
        .map_err(|error| error.to_string())?;
    let projection = IngressProjectionStore::new(store.clone())
        .load()
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "ingress endpoint projection is not initialized".to_owned())?;
    Ok(RuntimeIngressSources {
        ployz_dns_target_allocation: allocation,
        ployz_dns_target_checkpoint: checkpoint,
        ingress_endpoint_projection: projection.projection,
    })
}

pub(crate) fn from_sources(
    intent: IntentSnapshot,
    facts: &BTreeMap<MachineId, MachineFactsSnapshot>,
    gateway_statuses: &BTreeMap<MachineId, GatewayStatusObservation>,
    ingress: RuntimeIngressSources,
    read_at_unix_seconds: u64,
) -> RuntimeSnapshot {
    let ployz_dns_target = runtime_ployz_dns_target(
        intent.ployz_dns_target,
        ingress.ployz_dns_target_allocation,
        ingress.ployz_dns_target_checkpoint,
    );
    let route_tls = route_tls_statuses(&intent, read_at_unix_seconds);
    let machine_ids = intent
        .active_machines
        .iter()
        .map(|machine| machine.machine_id.clone())
        .collect::<Vec<_>>();
    let storage_testimony = facts
        .values()
        .map(|facts| {
            MachineStorageTestimony::new(facts.machine_id().clone(), facts.storage().cloned())
        })
        .collect::<Vec<_>>();
    let storage_alarms =
        derive_stranded_volume_alarms(&intent.volume_pins, &machine_ids, &storage_testimony);
    let machines = intent
        .active_machines
        .into_iter()
        .map(|active| machine_snapshot(active, facts, gateway_statuses, &storage_alarms))
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
        automatic_hostname_configuration: intent.automatic_hostname_configuration,
        ployz_dns_target,
        ingress_endpoint_projection: ingress.ingress_endpoint_projection,
        active_certificates: intent.active_certificates,
        route_tls,
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

fn runtime_ployz_dns_target(
    intent: PloyzDnsTargetIntent,
    allocation: Option<PloyzDnsTargetAllocation>,
    checkpoint: Option<ManagedDnsCheckpoint>,
) -> RuntimePloyzDnsTarget {
    let allocation = match allocation {
        Some(PloyzDnsTargetAllocation::Allocated { lease }) => {
            RuntimePloyzDnsTargetAllocation::Allocated {
                hostname: ployz_core::operation::RouteHostname::try_new(
                    lease.name.hostname_suffix(),
                )
                .expect("managed lease names always form valid route hostnames"),
                issued_at_unix_seconds: lease.issued_at.unix_seconds(),
                expires_at_unix_seconds: lease.expires_at.unix_seconds(),
            }
        }
        Some(PloyzDnsTargetAllocation::Unacquired { .. }) | None => {
            RuntimePloyzDnsTargetAllocation::Unacquired
        }
    };
    let publication = match checkpoint {
        Some(ManagedDnsCheckpoint::Applied {
            last_applied_identity,
            ..
        }) => RuntimePloyzDnsTargetPublication::Applied {
            ingress_projection: last_applied_identity,
        },
        Some(ManagedDnsCheckpoint::Withdrawn) => RuntimePloyzDnsTargetPublication::Withdrawn,
        None => RuntimePloyzDnsTargetPublication::Unpublished,
    };
    RuntimePloyzDnsTarget {
        intent,
        allocation,
        publication,
    }
}

fn route_tls_statuses(intent: &IntentSnapshot, read_at_unix_seconds: u64) -> Vec<RouteTlsStatus> {
    intent
        .route_bindings
        .iter()
        .map(|route| {
            let owner = CertificateOwner::RouteBinding {
                route_binding_id: route.id.clone(),
            };
            let certificate_id =
                usable_certificate(intent, &owner, read_at_unix_seconds).or_else(|| {
                    (route.origin == RouteBindingOrigin::Automatic
                        && intent.automatic_hostname_configuration
                            == ployz_core::ingress::AutomaticHostnameConfiguration::Ployz)
                        .then(|| {
                            usable_certificate(
                                intent,
                                &CertificateOwner::PloyzAutomaticNamespace,
                                read_at_unix_seconds,
                            )
                        })
                        .flatten()
                });
            RouteTlsStatus {
                route_binding_id: route.id.clone(),
                availability: certificate_id.map_or(RouteTlsAvailability::Unavailable, |id| {
                    RouteTlsAvailability::Available {
                        certificate_id: id.clone(),
                    }
                }),
            }
        })
        .collect()
}

fn usable_certificate<'a>(
    intent: &'a IntentSnapshot,
    owner: &CertificateOwner,
    read_at_unix_seconds: u64,
) -> Option<&'a ployz_core::ids::CertId> {
    intent
        .active_certificates
        .iter()
        .find(|certificate| {
            &certificate.owner == owner && certificate.active.is_usable_at(read_at_unix_seconds)
        })
        .map(|certificate| &certificate.active.cert_id)
}

fn machine_snapshot(
    active: ActiveMachineState,
    facts: &BTreeMap<MachineId, MachineFactsSnapshot>,
    gateways: &BTreeMap<MachineId, GatewayStatusObservation>,
    storage_alarms: &[ployz_core::machine::StrandedVolumeAlarm],
) -> MachineSnapshot {
    let testimony = match facts.get(&active.machine_id) {
        Some(facts) => MachineTestimony::Answered {
            endpoints: facts.endpoints().cloned(),
            gateway: gateways.get(&active.machine_id).cloned().map(Box::new),
            observed_container_count: facts.containers().containers().len(),
            disk_space: facts.disk_space(),
            storage: facts.storage().cloned(),
            last_observed_at_unix_seconds: facts.observed_at_unix_ms() / 1_000,
        },
        None => MachineTestimony::NoAnswer,
    };
    let alarms = storage_alarms
        .iter()
        .filter(|alarm| alarm.machine_id == active.machine_id)
        .cloned()
        .collect();
    MachineSnapshot {
        active,
        testimony,
        storage_alarms: alarms,
    }
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
    use ployz_core::certificate::{
        ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, LeaseBearerToken,
        LeaseExpiresAt, LeaseIssuedAt, ManagedLeaseName, ManagedLeaseRecord,
    };
    use ployz_core::ids::{CertId, RouteBindingId};
    use ployz_core::ingress::{
        ActiveCertificateMetadata, AutomaticHostnameConfiguration, IngressEndpointProjectionState,
    };
    use ployz_core::intent::recovery::ControlPlaneEpoch;
    use ployz_core::network::DataplaneProjection;
    use ployz_core::operation::{RouteHostname, RoutePort, RouteTarget};

    #[test]
    fn runtime_ingress_exposes_only_non_secret_current_state() {
        let lease = lease_record();
        let snapshot = from_sources(
            intent(Vec::new()),
            &BTreeMap::new(),
            &BTreeMap::new(),
            ingress(Some(PloyzDnsTargetAllocation::Allocated {
                lease: lease.clone(),
            })),
            1,
        );

        assert_eq!(
            snapshot.ployz_dns_target,
            RuntimePloyzDnsTarget {
                intent: PloyzDnsTargetIntent::Enabled,
                allocation: RuntimePloyzDnsTargetAllocation::Allocated {
                    hostname: route_hostname("brisk-river-x7f3.up.ployz.app"),
                    issued_at_unix_seconds: 1,
                    expires_at_unix_seconds: 100,
                },
                publication: RuntimePloyzDnsTargetPublication::Unpublished,
            }
        );
        let json = serde_json::to_string(&snapshot).expect("runtime JSON");
        assert!(!json.contains("lease-token"));
    }

    #[test]
    fn route_tls_uses_explicit_owner_and_validity() {
        let route = route("route_app", RouteBindingOrigin::Declared);
        let cert = active_certificate(CertificateOwner::RouteBinding {
            route_binding_id: route.id.clone(),
        });
        let snapshot = from_sources(
            intent(vec![(route.clone(), cert)]),
            &BTreeMap::new(),
            &BTreeMap::new(),
            ingress(None),
            50,
        );
        assert_eq!(
            snapshot.route_tls,
            vec![RouteTlsStatus {
                route_binding_id: route.id,
                availability: RouteTlsAvailability::Available {
                    certificate_id: CertId::try_new("cert_app_example_com")
                        .expect("certificate id"),
                },
            }]
        );
    }

    fn intent(
        routes_and_certificates: Vec<(RouteBindingState, ActiveCertificateMetadata)>,
    ) -> IntentSnapshot {
        let (route_bindings, active_certificates) = routes_and_certificates.into_iter().unzip();
        IntentSnapshot {
            epoch: ControlPlaneEpoch::initial(),
            core_machine_id: MachineId::try_new("core").expect("machine id"),
            active_machines: Vec::new(),
            dataplane_projection: DataplaneProjection::try_new(Vec::new(), None)
                .expect("dataplane projection"),
            route_bindings,
            serving_target_entries: Vec::new(),
            volume_pins: Vec::new(),
            nats_authorizations: Vec::new(),
            automatic_hostname_configuration: AutomaticHostnameConfiguration::Ployz,
            ployz_dns_target: PloyzDnsTargetIntent::Enabled,
            active_certificates,
        }
    }

    fn ingress(allocation: Option<PloyzDnsTargetAllocation>) -> RuntimeIngressSources {
        RuntimeIngressSources {
            ployz_dns_target_allocation: allocation,
            ployz_dns_target_checkpoint: None,
            ingress_endpoint_projection: IngressEndpointProjection {
                control_plane_epoch: ControlPlaneEpoch::initial(),
                revision: 0,
                state: IngressEndpointProjectionState::Pending,
            },
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

    fn route_hostname(value: &str) -> RouteHostname {
        RouteHostname::try_new(value).expect("route hostname")
    }

    fn route(id: &str, origin: RouteBindingOrigin) -> RouteBindingState {
        RouteBindingState {
            id: RouteBindingId::try_new(id).expect("route id"),
            namespace_id: NamespaceId::try_new("ns_app").expect("namespace id"),
            target: RouteTarget::new(route_hostname("app.example.com")),
            endpoint_port: RoutePort::try_new(8080).expect("port"),
            service_id: ServiceId::try_new("svc_app").expect("service id"),
            origin,
        }
    }

    fn active_certificate(owner: CertificateOwner) -> ActiveCertificateMetadata {
        ActiveCertificateMetadata {
            owner,
            active: ActiveCertState {
                cert_id: CertId::try_new("cert_app_example_com").expect("certificate id"),
                hostname: route_hostname("app.example.com"),
                bundle_ref: CertBundleRef::try_new(format!(
                    "sha256:{}:/var/lib/ployz/certificates/cert_app_example_com.bundle",
                    "a".repeat(64)
                ))
                .expect("bundle reference"),
                validity: CertValidityWindow::try_new(
                    CertValidAt::try_new(1).expect("not before"),
                    CertValidAt::try_new(100).expect("not after"),
                )
                .expect("validity window"),
            },
        }
    }
}
