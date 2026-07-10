//! Gateway projection read-model.

use ployz_core::cert::{AcmeHttp01Challenge, CustomCertBundle, ManagedCertBundle};
use ployz_core::ids::{ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, ServiceId};
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};

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
    pub managed_cert_bundle: Option<ManagedCertBundle>,
    pub custom_cert_bundles: Vec<CustomCertBundle>,
    pub custom_cert_failures: Vec<GatewayCertificateMaterialFailure>,
    pub challenges: Vec<AcmeHttp01Challenge>,
    pub routes: Vec<GatewayRoute>,
    pub serving: Vec<GatewayServingEntry>,
    /// Machines' self-reported container snapshots. Gateways serve every
    /// observed container an operation promoted: an offline machine's
    /// upstreams fail at dial time, which is the live and exact liveness
    /// answer — the projection never infers liveness from observation age
    /// (ADR 0027, superseding ADR 0009's staleness filter).
    pub observed_machines: Vec<MachineContainerObservationSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCertificateMaterialFailure {
    pub hostname: RouteHostname,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjection {
    pub managed_cert_bundle: Option<ManagedCertBundle>,
    pub custom_cert_bundles: Vec<CustomCertBundle>,
    pub challenges: Vec<AcmeHttp01Challenge>,
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
    DuplicateRouteTarget {
        target: RouteTarget,
    },
    CertificateMaterial {
        failures: Vec<GatewayCertificateMaterialFailure>,
    },
    InvalidSource {
        message: String,
    },
    SourceUnavailable {
        message: String,
    },
}

#[must_use]
pub fn apply_gateway_update(
    previous: GatewayProjectionState,
    update: GatewayProjectionUpdate,
) -> GatewayProjectionState {
    match update {
        GatewayProjectionUpdate::SourceAvailable(mut input) => {
            let failures = input.custom_cert_failures.clone();
            if let Some(last_good) = previous.last_good.as_ref() {
                for failure in &failures {
                    if input
                        .custom_cert_bundles
                        .iter()
                        .any(|bundle| bundle.active_cert().hostname == failure.hostname)
                    {
                        continue;
                    }
                    if let Some(bundle) = last_good
                        .custom_cert_bundles
                        .iter()
                        .find(|bundle| bundle.active_cert().hostname == failure.hostname)
                    {
                        input.custom_cert_bundles.push(bundle.clone());
                    }
                }
            }
            match project_gateway(input) {
                Ok(projection) => GatewayProjectionState {
                    last_good: Some(projection),
                    last_error: (!failures.is_empty())
                        .then_some(GatewayProjectionError::CertificateMaterial { failures }),
                },
                Err(error) => GatewayProjectionState {
                    last_error: Some(error),
                    ..previous
                },
            }
        }
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
    let managed_cert_bundle = input.managed_cert_bundle;
    let custom_cert_bundles = input.custom_cert_bundles;
    let custom_cert_failures = input.custom_cert_failures;
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
        if route.target.port.get() == 443
            && custom_cert_failures
                .iter()
                .any(|failure| failure.hostname == route.target.hostname)
            && !custom_cert_bundles
                .iter()
                .any(|bundle| bundle.active_cert().hostname == route.target.hostname)
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

    Ok(GatewayProjection {
        managed_cert_bundle,
        custom_cert_bundles,
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
