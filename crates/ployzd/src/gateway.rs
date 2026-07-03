//! Gateway projection runtime.

use ployz_core::ids::{ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, ServiceId};
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::ops::{RoutePort, RouteTarget};

use std::collections::BTreeMap;
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRoute {
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
}

/// One service entry of the current serving target: the entry identity whose
/// containers may serve that service (ADR 0023, ADR 0024).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayServingEntry {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjectionInput {
    pub routes: Vec<GatewayRoute>,
    pub serving: Vec<GatewayServingEntry>,
    pub observed_machines: Vec<GatewayMachineObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayMachineObservation {
    pub freshness: GatewayObservationFreshness,
    pub snapshot: MachineContainerObservationSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayObservationFreshness {
    Fresh,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjection {
    pub routes: Vec<GatewayProjectedRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjectedRoute {
    pub target: RouteTarget,
    pub upstreams: Vec<GatewayUpstream>,
    pub unroutable_containers: Vec<GatewayUnroutableContainer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayUpstream {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
    /// Container endpoint-network IP dialed on the route's endpoint port.
    pub address: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayUnroutableContainer {
    pub machine_id: MachineId,
    pub container_id: ContainerId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GatewayUpstreamKey {
    namespace_id: NamespaceId,
    service_id: ServiceId,
    namespace_revision_entry_id: NamespaceRevisionEntryId,
}

impl GatewayUpstreamKey {
    fn for_serving_entry(entry: &GatewayServingEntry) -> Self {
        Self {
            namespace_id: entry.namespace_id.clone(),
            service_id: entry.service_id.clone(),
            namespace_revision_entry_id: entry.namespace_revision_entry_id.clone(),
        }
    }

    fn for_container(container: &ployz_core::machine_runtime::ManagedContainerObservation) -> Self {
        Self {
            namespace_id: container.identity.namespace_id.clone(),
            service_id: container.identity.service_id.clone(),
            namespace_revision_entry_id: container.identity.namespace_revision_entry_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayProjectionUpdate {
    SourceAvailable(GatewayProjectionInput),
    SourceInvalid(GatewayProjectionError),
    SourceUnavailable(GatewayProjectionError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjectionState {
    pub last_good: Option<GatewayProjection>,
    pub last_error: Option<GatewayProjectionError>,
}

impl GatewayProjectionState {
    #[must_use]
    pub const fn unavailable() -> Self {
        Self {
            last_good: None,
            last_error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayProjectionError {
    DuplicateRouteTarget { target: RouteTarget },
    InvalidSource { message: String },
    SourceUnavailable { message: String },
}

#[must_use]
pub fn apply_gateway_update(
    previous: GatewayProjectionState,
    update: GatewayProjectionUpdate,
) -> GatewayProjectionState {
    match update {
        GatewayProjectionUpdate::SourceAvailable(input) => match project_gateway(input) {
            Ok(projection) => GatewayProjectionState {
                last_good: Some(projection),
                last_error: None,
            },
            Err(error) => GatewayProjectionState {
                last_error: Some(error),
                ..previous
            },
        },
        GatewayProjectionUpdate::SourceInvalid(error) => GatewayProjectionState {
            last_error: Some(error),
            ..previous
        },
        GatewayProjectionUpdate::SourceUnavailable(error) => {
            if previous.last_good.is_some() && previous.last_error.is_some() {
                previous
            } else {
                GatewayProjectionState {
                    last_error: Some(error),
                    ..previous
                }
            }
        }
    }
}

pub fn project_gateway(
    input: GatewayProjectionInput,
) -> Result<GatewayProjection, GatewayProjectionError> {
    let mut input_routes = input.routes;
    input_routes.sort_by(|left, right| left.target.cmp(&right.target));

    let mut previous_target = None;
    for route in &input_routes {
        if previous_target == Some(&route.target) {
            return Err(GatewayProjectionError::DuplicateRouteTarget {
                target: route.target.clone(),
            });
        }
        previous_target = Some(&route.target);
    }

    let serving_by_service = input
        .serving
        .iter()
        .map(|entry| {
            (
                (entry.namespace_id.clone(), entry.service_id.clone()),
                entry,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let indexed_containers = index_fresh_running_containers(&input.observed_machines);
    let mut routes = Vec::with_capacity(input_routes.len());
    for route in input_routes {
        // A binding whose service is absent from the serving target stays
        // attached with no upstreams; the gateway answers unavailable
        // instead of treating the route as invalid state (ADR 0024).
        let Some(serving_entry) =
            serving_by_service.get(&(route.namespace_id.clone(), route.service_id.clone()))
        else {
            routes.push(GatewayProjectedRoute {
                target: route.target,
                upstreams: Vec::new(),
                unroutable_containers: Vec::new(),
            });
            continue;
        };
        let key = GatewayUpstreamKey::for_serving_entry(serving_entry);
        let upstreams = indexed_containers
            .addresses_by_entry
            .get(&key)
            .map(|containers| {
                containers
                    .iter()
                    .map(|container| GatewayUpstream {
                        machine_id: container.machine_id.clone(),
                        container_id: container.container_id.clone(),
                        address: SocketAddr::new(container.ip, route.endpoint_port.get()),
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let unroutable_containers = indexed_containers
            .unroutable_by_entry
            .get(&key)
            .cloned()
            .unwrap_or_default();
        routes.push(GatewayProjectedRoute {
            target: route.target,
            upstreams,
            unroutable_containers,
        });
    }

    Ok(GatewayProjection { routes })
}

struct GatewayContainerAddress {
    machine_id: MachineId,
    container_id: ContainerId,
    ip: std::net::IpAddr,
}

struct IndexedGatewayContainers {
    addresses_by_entry: BTreeMap<GatewayUpstreamKey, Vec<GatewayContainerAddress>>,
    unroutable_by_entry: BTreeMap<GatewayUpstreamKey, Vec<GatewayUnroutableContainer>>,
}

fn index_fresh_running_containers(
    observed_machines: &[GatewayMachineObservation],
) -> IndexedGatewayContainers {
    let mut addresses_by_entry: BTreeMap<GatewayUpstreamKey, Vec<GatewayContainerAddress>> =
        BTreeMap::new();
    let mut unroutable_by_entry: BTreeMap<GatewayUpstreamKey, Vec<GatewayUnroutableContainer>> =
        BTreeMap::new();

    for container in observed_machines
        .iter()
        .filter(|machine| machine.freshness == GatewayObservationFreshness::Fresh)
        .flat_map(|machine| machine.snapshot.containers())
    {
        if !container.is_running_service() {
            continue;
        }
        let key = GatewayUpstreamKey::for_container(container);
        match container.running_service_ip() {
            Some(ip) => addresses_by_entry
                .entry(key)
                .or_default()
                .push(GatewayContainerAddress {
                    machine_id: container.machine_id.clone(),
                    container_id: container.container_id.clone(),
                    ip,
                }),
            None => unroutable_by_entry
                .entry(key)
                .or_default()
                .push(GatewayUnroutableContainer {
                    machine_id: container.machine_id.clone(),
                    container_id: container.container_id.clone(),
                }),
        }
    }

    for addresses in addresses_by_entry.values_mut() {
        addresses.sort_by(|left, right| {
            left.machine_id
                .cmp(&right.machine_id)
                .then_with(|| left.container_id.cmp(&right.container_id))
        });
    }

    for containers in unroutable_by_entry.values_mut() {
        containers.sort_by(|left, right| {
            left.machine_id
                .cmp(&right.machine_id)
                .then_with(|| left.container_id.cmp(&right.container_id))
        });
    }

    IndexedGatewayContainers {
        addresses_by_entry,
        unroutable_by_entry,
    }
}
