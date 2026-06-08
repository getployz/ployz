//! Gateway projection runtime.

use ployz_core::ids::{ContainerId, NodeId, RevisionId, ServiceId};
use ployz_core::node::{ContainerEndpoint, NodeContainerObservationSnapshot};
use ployz_core::ops::{RoutePort, RouteTarget};

use crate::projection::ProjectionState;

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRoute {
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
    pub service_id: ServiceId,
    pub revision_id: RevisionId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayProjectionInput {
    pub routes: Vec<GatewayRoute>,
    pub observed_nodes: Vec<GatewayNodeObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayNodeObservation {
    pub freshness: GatewayObservationFreshness,
    pub snapshot: NodeContainerObservationSnapshot,
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
    pub node_id: NodeId,
    pub container_id: ContainerId,
    pub endpoint: ContainerEndpoint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayUnroutableContainer {
    pub node_id: NodeId,
    pub container_id: ContainerId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GatewayUpstreamKey {
    service_id: ServiceId,
    revision_id: RevisionId,
    endpoint_port: RoutePort,
}

impl GatewayUpstreamKey {
    fn for_route(route: &GatewayRoute) -> Self {
        Self {
            service_id: route.service_id.clone(),
            revision_id: route.revision_id.clone(),
            endpoint_port: route.endpoint_port,
        }
    }

    fn for_container(
        container: &ployz_core::node::ManagedContainerObservation,
        endpoint_port: RoutePort,
    ) -> Self {
        Self {
            service_id: container.service_id.clone(),
            revision_id: container.revision_id.clone(),
            endpoint_port,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayProjectionUpdate {
    SourceAvailable(GatewayProjectionInput),
    SourceInvalid(GatewayProjectionError),
    SourceUnavailable(GatewayProjectionError),
}

pub type GatewayProjectionState = ProjectionState<GatewayProjection, GatewayProjectionError>;

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
            Ok(projection) => GatewayProjectionState::Current(projection),
            Err(error) => previous.source_failed(error),
        },
        GatewayProjectionUpdate::SourceInvalid(error) => previous.source_failed(error),
        GatewayProjectionUpdate::SourceUnavailable(error) => {
            previous.source_unavailable_with_error(error)
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

    let indexed_containers = index_fresh_running_containers(&input.observed_nodes);
    let mut routes = Vec::with_capacity(input_routes.len());
    for route in input_routes {
        let key = GatewayUpstreamKey::for_route(&route);
        let upstreams = indexed_containers
            .upstreams_by_revision
            .get(&key)
            .cloned()
            .unwrap_or_default();
        let unroutable_containers = indexed_containers
            .unroutable_by_revision
            .get(&GatewayRevisionKey::for_route(&route))
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

struct IndexedGatewayContainers {
    upstreams_by_revision: BTreeMap<GatewayUpstreamKey, Vec<GatewayUpstream>>,
    unroutable_by_revision: BTreeMap<GatewayRevisionKey, Vec<GatewayUnroutableContainer>>,
}

fn index_fresh_running_containers(
    observed_nodes: &[GatewayNodeObservation],
) -> IndexedGatewayContainers {
    let mut upstreams_by_revision: BTreeMap<GatewayUpstreamKey, Vec<GatewayUpstream>> =
        BTreeMap::new();
    let mut unroutable_by_revision: BTreeMap<GatewayRevisionKey, Vec<GatewayUnroutableContainer>> =
        BTreeMap::new();

    for container in observed_nodes
        .iter()
        .filter(|node| node.freshness == GatewayObservationFreshness::Fresh)
        .flat_map(|node| node.snapshot.containers())
    {
        if !container.is_running_service() {
            continue;
        }
        match container.running_service_endpoint() {
            Some(endpoint) => {
                let key = GatewayUpstreamKey::for_container(container, endpoint.port);
                upstreams_by_revision
                    .entry(key)
                    .or_default()
                    .push(GatewayUpstream {
                        node_id: container.node_id.clone(),
                        container_id: container.container_id.clone(),
                        endpoint: endpoint.clone(),
                    })
            }
            None => {
                let key = GatewayRevisionKey::for_container(container);
                unroutable_by_revision
                    .entry(key)
                    .or_default()
                    .push(GatewayUnroutableContainer {
                        node_id: container.node_id.clone(),
                        container_id: container.container_id.clone(),
                    })
            }
        }
    }

    for upstreams in upstreams_by_revision.values_mut() {
        upstreams.sort_by(|left, right| {
            left.node_id
                .cmp(&right.node_id)
                .then_with(|| left.container_id.cmp(&right.container_id))
        });
    }

    for containers in unroutable_by_revision.values_mut() {
        containers.sort_by(|left, right| {
            left.node_id
                .cmp(&right.node_id)
                .then_with(|| left.container_id.cmp(&right.container_id))
        });
    }

    IndexedGatewayContainers {
        upstreams_by_revision,
        unroutable_by_revision,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GatewayRevisionKey {
    service_id: ServiceId,
    revision_id: RevisionId,
}

impl GatewayRevisionKey {
    fn for_route(route: &GatewayRoute) -> Self {
        Self {
            service_id: route.service_id.clone(),
            revision_id: route.revision_id.clone(),
        }
    }

    fn for_container(container: &ployz_core::node::ManagedContainerObservation) -> Self {
        Self {
            service_id: container.service_id.clone(),
            revision_id: container.revision_id.clone(),
        }
    }
}
