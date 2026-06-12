//! Load deploy execution facts from current-state stores.

use futures_util::{StreamExt, stream};
use ployz_core::dataplane::{DEFAULT_WIREGUARD_LISTEN_PORT, WireGuardPeerEndpoint};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{NodeId, ServiceId};
use ployz_core::node::NodeContainerObservationSnapshot;
use ployz_core::state::NodePublicIpObservation;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use super::DeployExecutionFacts;

const MAX_CONCURRENT_OBSERVATION_READS: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployExecutionNodeScope {
    pub eligible_nodes: Vec<NodeId>,
    pub observed_node_ids: Vec<NodeId>,
}

impl DeployExecutionNodeScope {
    #[must_use]
    pub fn same_nodes(nodes: Vec<NodeId>) -> Self {
        Self {
            eligible_nodes: nodes.clone(),
            observed_node_ids: nodes,
        }
    }
}

pub async fn load_deploy_execution_facts_from_nats(
    request: &DeployRequest,
    node_scope: DeployExecutionNodeScope,
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
    step_timeout: Duration,
) -> Result<DeployExecutionFacts, DeployFactLoadError> {
    let active_service = core_state
        .active_service(&request.service_id)
        .await
        .map_err(|source| DeployFactLoadError::ActiveServiceRead {
            service_id: request.service_id.clone(),
            message: source.to_string(),
        })?;
    let active_route =
        match &request.route {
            Some(route) => Some(core_state.active_route(&route.target).await.map_err(
                |source| DeployFactLoadError::ActiveRouteRead {
                    route: route.target.clone(),
                    message: source.to_string(),
                },
            )?),
            None => None,
        }
        .flatten();
    let node_scope = load_active_machine_node_scope(core_state, node_scope).await?;
    let observed_nodes = load_node_snapshots(observations, &node_scope.observed_node_ids).await?;
    let dataplane_nodes = routed_dataplane_nodes(request, node_scope.observed_node_ids.clone());
    let peer_endpoint_node_ids = sorted_unique_nodes(
        node_scope
            .eligible_nodes
            .iter()
            .chain(dataplane_nodes.iter()),
    );
    let public_ip_requirement = match request.route {
        Some(_) => NodePublicIpRequirement::RequireAll,
        None => NodePublicIpRequirement::BestEffort,
    };
    let wireguard_peer_endpoints =
        load_wireguard_peer_endpoints(observations, &peer_endpoint_node_ids, public_ip_requirement)
            .await?;

    Ok(DeployExecutionFacts {
        active_service,
        active_route,
        eligible_nodes: node_scope.eligible_nodes,
        dataplane_nodes,
        observed_nodes,
        wireguard_peer_endpoints,
        step_timeout,
    })
}

fn routed_dataplane_nodes(request: &DeployRequest, fallback_nodes: Vec<NodeId>) -> Vec<NodeId> {
    if request.route.is_none() {
        return Vec::new();
    }

    sorted_unique_nodes(fallback_nodes.iter())
}

async fn load_active_machine_node_scope(
    core_state: &AsyncNatsCoreStateStore,
    fallback: DeployExecutionNodeScope,
) -> Result<DeployExecutionNodeScope, DeployFactLoadError> {
    let machines = core_state.active_machines().await.map_err(|source| {
        DeployFactLoadError::ActiveMachineRead {
            message: source.to_string(),
        }
    })?;
    if machines.is_empty() {
        return Ok(fallback);
    }

    Ok(DeployExecutionNodeScope::same_nodes(
        machines
            .into_iter()
            .map(|machine| machine.node_id)
            .collect(),
    ))
}

async fn load_node_snapshots(
    observations: &AsyncNatsObservationStore,
    node_ids: &[NodeId],
) -> Result<Vec<NodeContainerObservationSnapshot>, DeployFactLoadError> {
    let mut snapshots = Vec::new();
    let mut reads = stream::iter(node_ids.iter().cloned())
        .map(|node_id| async move {
            observations
                .node_snapshot(&node_id)
                .await
                .map_err(|source| DeployFactLoadError::NodeObservationRead {
                    node_id,
                    message: source.to_string(),
                })
        })
        .buffer_unordered(MAX_CONCURRENT_OBSERVATION_READS);

    while let Some(snapshot) = reads.next().await {
        if let Some(snapshot) = snapshot? {
            snapshots.push(snapshot);
        }
    }

    Ok(snapshots)
}

async fn load_wireguard_peer_endpoints(
    observations: &AsyncNatsObservationStore,
    node_ids: &[NodeId],
    requirement: NodePublicIpRequirement,
) -> Result<Vec<WireGuardPeerEndpoint>, DeployFactLoadError> {
    let requested = node_ids.iter().collect::<std::collections::BTreeSet<_>>();
    let mut endpoints = observations
        .node_public_ips()
        .await
        .map_err(|source| DeployFactLoadError::NodePublicIpObservationRead {
            message: source.to_string(),
        })?
        .into_iter()
        .filter(|observation| requested.contains(&observation.node_id))
        .map(peer_endpoint_from_public_ip)
        .collect::<Vec<_>>();

    endpoints.sort_by(|left, right| left.node_id.cmp(&right.node_id));
    if requirement == NodePublicIpRequirement::RequireAll {
        for node_id in node_ids {
            if endpoints
                .iter()
                .all(|endpoint| endpoint.node_id != *node_id)
            {
                return Err(DeployFactLoadError::MissingNodePublicIpObservation {
                    node_id: node_id.clone(),
                });
            }
        }
    }
    Ok(endpoints)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NodePublicIpRequirement {
    BestEffort,
    RequireAll,
}

fn peer_endpoint_from_public_ip(observation: NodePublicIpObservation) -> WireGuardPeerEndpoint {
    WireGuardPeerEndpoint::new(
        observation.node_id,
        SocketAddr::new(observation.public_ip, DEFAULT_WIREGUARD_LISTEN_PORT),
    )
}

fn sorted_unique_nodes<'a>(nodes: impl IntoIterator<Item = &'a NodeId>) -> Vec<NodeId> {
    nodes
        .into_iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// A current-state read failed before deploy execution started. Each variant
/// carries the rendered store-error message as failure evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployFactLoadError {
    ActiveServiceRead {
        service_id: ServiceId,
        message: String,
    },
    ActiveRouteRead {
        route: ployz_core::ops::RouteTarget,
        message: String,
    },
    ActiveMachineRead {
        message: String,
    },
    NodeObservationRead {
        node_id: NodeId,
        message: String,
    },
    NodePublicIpObservationRead {
        message: String,
    },
    MissingNodePublicIpObservation {
        node_id: NodeId,
    },
}

impl fmt::Display for DeployFactLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveServiceRead {
                service_id,
                message,
            } => write!(
                formatter,
                "active service state for {} could not be read: {}",
                service_id.as_str(),
                message
            ),
            Self::ActiveRouteRead { route, message } => write!(
                formatter,
                "active route state for {:?} could not be read: {}",
                route, message
            ),
            Self::ActiveMachineRead { message } => {
                write!(formatter, "active machines could not be read: {message}")
            }
            Self::NodeObservationRead { node_id, message } => write!(
                formatter,
                "node observations for {} could not be read: {}",
                node_id.as_str(),
                message
            ),
            Self::NodePublicIpObservationRead { message } => {
                write!(
                    formatter,
                    "node public ip observations could not be read: {message}"
                )
            }
            Self::MissingNodePublicIpObservation { node_id } => write!(
                formatter,
                "node public ip observation for {} is missing",
                node_id.as_str()
            ),
        }
    }
}
