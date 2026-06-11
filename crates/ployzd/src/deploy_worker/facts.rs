//! Load deploy execution facts from current-state stores.

use futures_util::{StreamExt, stream};
use ployz_core::dataplane::{DEFAULT_WIREGUARD_LISTEN_PORT, WireGuardPeerEndpoint};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{NodeId, ServiceId};
use ployz_core::node::NodeContainerObservationSnapshot;
use ployz_core::state::NodePublicIpObservation;
use ployz_nats::core_state::{
    ActiveMachineReadError, ActiveRouteReadError, AsyncNatsCoreStateStore, CoreStateStoreError,
};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
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
            failure: active_service_read_failure(source),
        })?;
    let active_route =
        match &request.route {
            Some(route) => Some(core_state.active_route(&route.target).await.map_err(
                |source| DeployFactLoadError::ActiveRouteRead {
                    route: route.target.clone(),
                    failure: active_route_read_failure(source),
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
            failure: active_machine_read_failure(source),
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
                    failure: observation_read_failure(source),
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
            failure: observation_read_failure(source),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployFactLoadError {
    ActiveServiceRead {
        service_id: ServiceId,
        failure: ActiveServiceReadFailure,
    },
    ActiveRouteRead {
        route: ployz_core::ops::RouteTarget,
        failure: ActiveRouteReadFailure,
    },
    ActiveMachineRead {
        failure: ActiveMachineReadFailure,
    },
    NodeObservationRead {
        node_id: NodeId,
        failure: ObservationReadFailure,
    },
    NodePublicIpObservationRead {
        failure: ObservationReadFailure,
    },
    MissingNodePublicIpObservation {
        node_id: NodeId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveServiceReadFailure {
    OpenBucket {
        bucket: &'static str,
        message: String,
    },
    Decode {
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    CorruptState {
        key: String,
        expected_service_id: ServiceId,
        actual_service_id: ServiceId,
    },
    Timeout {
        operation: &'static str,
    },
    UnexpectedWriteFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveRouteReadFailure {
    Decode { message: String },
    ListKeys { message: String },
    Watch { message: String },
    Get { key: String, message: String },
    CorruptState { key: String, message: String },
    Timeout { operation: &'static str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationReadFailure {
    OpenBucket {
        bucket: &'static str,
        message: String,
    },
    Decode {
        message: String,
    },
    ListKeys {
        message: String,
    },
    Watch {
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    CorruptSnapshotKey {
        key: String,
        actual_key: String,
    },
    Timeout {
        operation: &'static str,
    },
    UnexpectedWriteFailure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveMachineReadFailure {
    Decode {
        message: String,
    },
    ListKeys {
        message: String,
    },
    Get {
        key: String,
        message: String,
    },
    CorruptState {
        key: String,
        expected_node_id: NodeId,
    },
    Timeout {
        operation: &'static str,
    },
}

fn active_machine_read_failure(source: ActiveMachineReadError) -> ActiveMachineReadFailure {
    match source {
        ActiveMachineReadError::Decode(error) => ActiveMachineReadFailure::Decode {
            message: error.to_string(),
        },
        ActiveMachineReadError::Get { key, message } => {
            ActiveMachineReadFailure::Get { key, message }
        }
        ActiveMachineReadError::ListKeys { message } => {
            ActiveMachineReadFailure::ListKeys { message }
        }
        ActiveMachineReadError::CorruptActiveMachineState {
            key,
            expected_node_id,
        } => ActiveMachineReadFailure::CorruptState {
            key,
            expected_node_id,
        },
        ActiveMachineReadError::Timeout { operation } => {
            ActiveMachineReadFailure::Timeout { operation }
        }
    }
}

fn active_route_read_failure(source: ActiveRouteReadError) -> ActiveRouteReadFailure {
    match source {
        ActiveRouteReadError::Decode(error) => ActiveRouteReadFailure::Decode {
            message: error.to_string(),
        },
        ActiveRouteReadError::ListKeys { message } => ActiveRouteReadFailure::ListKeys { message },
        ActiveRouteReadError::Watch { message } => ActiveRouteReadFailure::Watch { message },
        ActiveRouteReadError::Get { key, message } => ActiveRouteReadFailure::Get { key, message },
        ActiveRouteReadError::CorruptActiveRouteState {
            key,
            expected_target,
            actual_target,
        } => ActiveRouteReadFailure::CorruptState {
            key,
            message: format!(
                "active route state belongs to {actual_target:?}, not {expected_target:?}"
            ),
        },
        ActiveRouteReadError::CorruptActiveRouteKey { key, actual_key } => {
            ActiveRouteReadFailure::CorruptState {
                key,
                message: format!("active route key does not match encoded target {actual_key}"),
            }
        }
        ActiveRouteReadError::Timeout { operation } => {
            ActiveRouteReadFailure::Timeout { operation }
        }
    }
}

fn active_service_read_failure(source: CoreStateStoreError) -> ActiveServiceReadFailure {
    match source {
        CoreStateStoreError::OpenBucket { bucket, message } => {
            ActiveServiceReadFailure::OpenBucket { bucket, message }
        }
        CoreStateStoreError::Decode(error) => ActiveServiceReadFailure::Decode {
            message: error.to_string(),
        },
        CoreStateStoreError::Get { key, message } => ActiveServiceReadFailure::Get { key, message },
        CoreStateStoreError::ListKeys { message } => ActiveServiceReadFailure::Get {
            key: "services.*".to_owned(),
            message,
        },
        CoreStateStoreError::CorruptActiveServiceState {
            key,
            expected_service_id,
            actual_service_id,
        } => ActiveServiceReadFailure::CorruptState {
            key,
            expected_service_id,
            actual_service_id,
        },
        CoreStateStoreError::Timeout { operation } => {
            ActiveServiceReadFailure::Timeout { operation }
        }
        CoreStateStoreError::Encode(_) | CoreStateStoreError::CasConflict { .. } => {
            ActiveServiceReadFailure::UnexpectedWriteFailure
        }
    }
}

fn observation_read_failure(source: ObservationStoreError) -> ObservationReadFailure {
    match source {
        ObservationStoreError::OpenBucket { bucket, message } => {
            ObservationReadFailure::OpenBucket { bucket, message }
        }
        ObservationStoreError::Decode(error) => ObservationReadFailure::Decode {
            message: error.to_string(),
        },
        ObservationStoreError::ListKeys { message } => ObservationReadFailure::ListKeys { message },
        ObservationStoreError::Watch { message } => ObservationReadFailure::Watch { message },
        ObservationStoreError::Get { key, message } => ObservationReadFailure::Get { key, message },
        ObservationStoreError::CorruptNodeSnapshotKey { key, actual_key } => {
            ObservationReadFailure::CorruptSnapshotKey { key, actual_key }
        }
        ObservationStoreError::CorruptNodePublicIpKey { key, actual_key }
        | ObservationStoreError::CorruptGatewayStatusKey { key, actual_key } => {
            ObservationReadFailure::CorruptSnapshotKey { key, actual_key }
        }
        ObservationStoreError::Timeout { operation } => {
            ObservationReadFailure::Timeout { operation }
        }
        ObservationStoreError::Encode(_)
        | ObservationStoreError::Put { .. }
        | ObservationStoreError::Delete { .. } => ObservationReadFailure::UnexpectedWriteFailure,
    }
}

impl fmt::Display for DeployFactLoadError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActiveServiceRead {
                service_id,
                failure,
            } => write!(
                formatter,
                "active service state for {} could not be read: {}",
                service_id.as_str(),
                failure
            ),
            Self::ActiveRouteRead { route, failure } => write!(
                formatter,
                "active route state for {:?} could not be read: {}",
                route, failure
            ),
            Self::ActiveMachineRead { failure } => {
                write!(formatter, "active machines could not be read: {failure}")
            }
            Self::NodeObservationRead { node_id, failure } => write!(
                formatter,
                "node observations for {} could not be read: {}",
                node_id.as_str(),
                failure
            ),
            Self::NodePublicIpObservationRead { failure } => {
                write!(
                    formatter,
                    "node public ip observations could not be read: {failure}"
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

impl fmt::Display for ActiveServiceReadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenBucket { bucket, message } => {
                write!(formatter, "open bucket {bucket}: {message}")
            }
            Self::Decode { message } => {
                write!(formatter, "decode active service state: {message}")
            }
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::CorruptState {
                key,
                expected_service_id,
                actual_service_id,
            } => write!(
                formatter,
                "state at {} belongs to {}, not {}",
                key,
                actual_service_id.as_str(),
                expected_service_id.as_str()
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
            Self::UnexpectedWriteFailure => {
                formatter.write_str("unexpected write-path failure during read")
            }
        }
    }
}

impl fmt::Display for ActiveMachineReadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode { message } => write!(formatter, "decode active machine state: {message}"),
            Self::ListKeys { message } => write!(formatter, "list active machine keys: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::CorruptState {
                key,
                expected_node_id,
            } => write!(
                formatter,
                "state at {} does not belong to {}",
                key,
                expected_node_id.as_str()
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

impl fmt::Display for ActiveRouteReadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode { message } => write!(formatter, "decode active route state: {message}"),
            Self::ListKeys { message } => write!(formatter, "list active route keys: {message}"),
            Self::Watch { message } => write!(formatter, "watch active route keys: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::CorruptState { key, message } => {
                write!(formatter, "corrupt route state at {key}: {message}")
            }
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
        }
    }
}

impl fmt::Display for ObservationReadFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OpenBucket { bucket, message } => {
                write!(formatter, "open bucket {bucket}: {message}")
            }
            Self::Decode { message } => {
                write!(formatter, "decode observation snapshot: {message}")
            }
            Self::ListKeys { message } => write!(formatter, "list observation keys: {message}"),
            Self::Watch { message } => write!(formatter, "watch observation keys: {message}"),
            Self::Get { key, message } => write!(formatter, "get {key}: {message}"),
            Self::CorruptSnapshotKey { key, actual_key } => write!(
                formatter,
                "snapshot key {} does not match decoded snapshot key {}",
                key, actual_key
            ),
            Self::Timeout { operation } => write!(formatter, "{operation} timed out"),
            Self::UnexpectedWriteFailure => {
                formatter.write_str("unexpected write-path failure during read")
            }
        }
    }
}
