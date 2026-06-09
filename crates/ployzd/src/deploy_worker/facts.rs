//! Load deploy execution facts from current-state stores.

use futures_util::{StreamExt, stream};
use ployz_core::deploy::DeployRequest;
use ployz_core::ids::{NodeId, ServiceId};
use ployz_core::node::NodeContainerObservationSnapshot;
use ployz_nats::core_state::{ActiveRouteReadError, AsyncNatsCoreStateStore, CoreStateStoreError};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use std::fmt;
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
    let observed_nodes = load_node_snapshots(observations, &node_scope.observed_node_ids).await?;

    Ok(DeployExecutionFacts {
        active_service,
        active_route,
        eligible_nodes: node_scope.eligible_nodes,
        observed_nodes,
        step_timeout,
    })
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
    NodeObservationRead {
        node_id: NodeId,
        failure: ObservationReadFailure,
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
            Self::NodeObservationRead { node_id, failure } => write!(
                formatter,
                "node observations for {} could not be read: {}",
                node_id.as_str(),
                failure
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
