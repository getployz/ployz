//! Gateway projection read-model.

use ployz_core::certificate::{AcmeHttp01Challenge, CustomCertBundle};
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, RouteBindingId, ServiceId,
};
use ployz_core::ingress::{CertificateOwner, RouteBindingOrigin};
use ployz_core::machine::runtime::MachineContainerObservationSnapshot;
use ployz_core::operation::{RoutePort, RouteTarget};

use std::collections::BTreeMap;
use std::net::SocketAddr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRoute {
    pub id: RouteBindingId,
    pub origin: RouteBindingOrigin,
    pub certificate_owner: CertificateOwner,
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
    pub certificate_bundles: Vec<GatewayCertificateBundle>,
    pub certificate_failures: Vec<GatewayCertificateMaterialFailure>,
    pub challenges: Vec<AcmeHttp01Challenge>,
    pub routes: Vec<GatewayRoute>,
    pub serving: Vec<GatewayServingEntry>,
    /// Machines' self-reported container snapshots. Gateways serve every
    /// observed container an operation promoted: an offline machine's
    /// upstreams fail at dial time, which is the live and exact liveness
    /// answer — the projection never infers liveness from observation age
    /// (ADR 0040, superseding ADR 0009's staleness filter).
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCertificateMaterialFailure {
    pub owner: CertificateOwner,
    pub kind: GatewayCertificateMaterialFailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayCertificateMaterialFailureKind {
    MissingOrInvalid,
    Unusable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCertificateBundle {
    pub owner: CertificateOwner,
    pub bundle: CustomCertBundle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjection {
    pub certificate_bundles: Vec<GatewayCertificateBundle>,
    pub challenges: Vec<AcmeHttp01Challenge>,
    pub routes: Vec<GatewayProjectedRoute>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjectedRoute {
    pub id: RouteBindingId,
    pub origin: RouteBindingOrigin,
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

    fn for_container(
        container: &ployz_core::machine::runtime::ManagedContainerObservation,
    ) -> Self {
        Self {
            namespace_id: container.identity.namespace_id.clone(),
            service_id: container.identity.service_id.clone(),
            namespace_revision_entry_id: container.identity.namespace_revision_entry_id.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayProjectionUpdate {
    Available(Box<GatewayProjectionInput>),
    #[cfg(test)]
    Invalid(GatewayProjectionError),
    Unavailable(GatewayProjectionError),
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
    DuplicateRouteTarget {
        target: RouteTarget,
    },
    CertificateMaterial {
        failures: Vec<GatewayCertificateMaterialFailure>,
        availability: GatewayCertificateFailureAvailability,
    },
    #[cfg(test)]
    InvalidSource {
        message: String,
    },
    SourceUnavailable {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayCertificateFailureAvailability {
    Unavailable,
    ServingLastKnownGood,
}

#[must_use]
pub fn apply_gateway_update(
    previous: GatewayProjectionState,
    update: GatewayProjectionUpdate,
) -> GatewayProjectionState {
    match update {
        GatewayProjectionUpdate::Available(input) => {
            let mut input = *input;
            let failures = input.certificate_failures.clone();
            let had_complete_projection = previous.last_good.is_some()
                && !matches!(
                    &previous.last_error,
                    Some(GatewayProjectionError::CertificateMaterial {
                        availability: GatewayCertificateFailureAvailability::Unavailable,
                        ..
                    })
                );
            if let Some(last_good) = previous.last_good.as_ref() {
                for failure in &failures {
                    if failure.kind == GatewayCertificateMaterialFailureKind::Unusable {
                        continue;
                    }
                    if input
                        .certificate_bundles
                        .iter()
                        .any(|bundle| bundle.owner == failure.owner)
                    {
                        continue;
                    }
                    if let Some(bundle) = last_good
                        .certificate_bundles
                        .iter()
                        .find(|bundle| bundle.owner == failure.owner)
                    {
                        input.certificate_bundles.push(bundle.clone());
                    }
                }
            }
            match project_gateway(input) {
                Ok(projection) => GatewayProjectionState {
                    last_good: Some(projection),
                    last_error: (!failures.is_empty()).then_some(
                        GatewayProjectionError::CertificateMaterial {
                            failures,
                            availability: if had_complete_projection {
                                GatewayCertificateFailureAvailability::ServingLastKnownGood
                            } else {
                                GatewayCertificateFailureAvailability::Unavailable
                            },
                        },
                    ),
                },
                Err(error) => GatewayProjectionState {
                    last_error: Some(error),
                    ..previous
                },
            }
        }
        #[cfg(test)]
        GatewayProjectionUpdate::Invalid(error) => GatewayProjectionState {
            last_error: Some(error),
            ..previous
        },
        GatewayProjectionUpdate::Unavailable(error) => {
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
    let certificate_bundles = input.certificate_bundles;
    let certificate_failures = input.certificate_failures;
    let challenges = input.challenges;
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
    let indexed_containers = index_running_containers(&input.observed_machines);
    let mut routes = Vec::with_capacity(input_routes.len());
    for route in input_routes {
        let certificate_owner = route.certificate_owner.clone();
        if certificate_failures
            .iter()
            .any(|failure| failure.owner == certificate_owner)
            && !certificate_bundles
                .iter()
                .any(|bundle| bundle.owner == certificate_owner)
        {
            continue;
        }
        // A binding whose service is absent from the serving target stays
        // attached with no upstreams; the gateway answers unavailable
        // instead of treating the route as invalid state (ADR 0024).
        let Some(serving_entry) =
            serving_by_service.get(&(route.namespace_id.clone(), route.service_id.clone()))
        else {
            routes.push(GatewayProjectedRoute {
                id: route.id,
                origin: route.origin,
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
            id: route.id,
            origin: route.origin,
            target: route.target,
            upstreams,
            unroutable_containers,
        });
    }

    Ok(GatewayProjection {
        certificate_bundles,
        challenges,
        routes,
    })
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

fn index_running_containers(
    observed_machines: &[MachineContainerObservationSnapshot],
) -> IndexedGatewayContainers {
    let mut addresses_by_entry: BTreeMap<GatewayUpstreamKey, Vec<GatewayContainerAddress>> =
        BTreeMap::new();
    let mut unroutable_by_entry: BTreeMap<GatewayUpstreamKey, Vec<GatewayUnroutableContainer>> =
        BTreeMap::new();

    for container in observed_machines
        .iter()
        .flat_map(MachineContainerObservationSnapshot::containers)
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

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
