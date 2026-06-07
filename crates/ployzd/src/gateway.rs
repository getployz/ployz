//! Gateway projection runtime.

use ployz_core::ids::{ContainerId, NodeId, RevisionId, ServiceId};
use ployz_core::node::NodeContainerObservationSnapshot;
use ployz_core::ops::RouteTarget;

use crate::projection::ProjectionState;

use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRoute {
    pub target: RouteTarget,
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayUpstream {
    pub node_id: NodeId,
    pub container_id: ContainerId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GatewayUpstreamKey {
    service_id: ServiceId,
    revision_id: RevisionId,
}

impl GatewayUpstreamKey {
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

    let upstreams_by_revision = index_fresh_running_upstreams(&input.observed_nodes);
    let mut routes = Vec::with_capacity(input_routes.len());
    for route in input_routes {
        let upstreams = upstreams_by_revision
            .get(&GatewayUpstreamKey::for_route(&route))
            .cloned()
            .unwrap_or_default();
        routes.push(GatewayProjectedRoute {
            target: route.target,
            upstreams,
        });
    }

    Ok(GatewayProjection { routes })
}

fn index_fresh_running_upstreams(
    observed_nodes: &[GatewayNodeObservation],
) -> BTreeMap<GatewayUpstreamKey, Vec<GatewayUpstream>> {
    let mut upstreams_by_revision: BTreeMap<GatewayUpstreamKey, Vec<GatewayUpstream>> =
        BTreeMap::new();

    for container in observed_nodes
        .iter()
        .filter(|node| node.freshness == GatewayObservationFreshness::Fresh)
        .flat_map(|node| node.snapshot.containers())
        .filter(|container| container.is_running_service())
    {
        upstreams_by_revision
            .entry(GatewayUpstreamKey::for_container(container))
            .or_default()
            .push(GatewayUpstream {
                node_id: container.node_id.clone(),
                container_id: container.container_id.clone(),
            });
    }

    for upstreams in upstreams_by_revision.values_mut() {
        upstreams.sort_by(|left, right| {
            left.node_id
                .cmp(&right.node_id)
                .then_with(|| left.container_id.cmp(&right.container_id))
        });
    }

    upstreams_by_revision
}
