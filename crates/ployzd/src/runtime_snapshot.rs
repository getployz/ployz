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
    MachineSnapshot, MachineTestimony, RuntimeDerivedCollectionSource,
    RuntimeDerivedCollectionStatus, RuntimeProjectionSource, RuntimeProjectionSources,
    RuntimeServiceInstance, RuntimeServiceRelease, RuntimeServiceRevision, RuntimeSnapshot,
    ServiceContainerMembership, ServiceContainerTestimony, ServiceMachineTestimony,
    ServiceSnapshot, ServiceTestimony,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn from_sources(
    intent: IntentSnapshot,
    facts: &BTreeMap<MachineId, MachineFactsSnapshot>,
    gateway_statuses: &BTreeMap<MachineId, GatewayStatusObservation>,
    read_at_unix_seconds: u64,
) -> RuntimeSnapshot {
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
