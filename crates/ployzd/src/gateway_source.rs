//! Gateway projection source adapters.

use crate::gateway::{
    GatewayNodeObservation, GatewayObservationFreshness, GatewayProjectionError,
    GatewayProjectionInput, GatewayProjectionUpdate, GatewayRoute,
};
use ployz_core::state::ActiveRouteState;
use ployz_nats::core_state::{ActiveRouteStoreError, AsyncNatsCoreStateStore};
use ployz_nats::observations::{
    AsyncNatsObservationStore, NodeContainerObservationRecord, ObservationStoreError,
};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_GATEWAY_OBSERVATION_STALE_AFTER: Duration = Duration::from_secs(30);

pub async fn load_gateway_projection_update_from_nats(
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
) -> GatewayProjectionUpdate {
    load_gateway_projection_update_from_nats_with_stale_after(
        core_state,
        observations,
        DEFAULT_GATEWAY_OBSERVATION_STALE_AFTER,
    )
    .await
}

pub async fn load_gateway_projection_update_from_nats_with_stale_after(
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
    stale_after: Duration,
) -> GatewayProjectionUpdate {
    match load_gateway_projection_input_from_nats_with_stale_after(
        core_state,
        observations,
        stale_after,
    )
    .await
    {
        Ok(input) => GatewayProjectionUpdate::SourceAvailable(input),
        Err(GatewaySourceError::Invalid { message }) => {
            GatewayProjectionUpdate::SourceInvalid(GatewayProjectionError::InvalidSource {
                message,
            })
        }
        Err(GatewaySourceError::Unavailable { message }) => {
            GatewayProjectionUpdate::SourceUnavailable(GatewayProjectionError::SourceUnavailable {
                message,
            })
        }
    }
}

pub async fn load_gateway_projection_input_from_nats(
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
) -> Result<GatewayProjectionInput, GatewaySourceError> {
    load_gateway_projection_input_from_nats_with_stale_after(
        core_state,
        observations,
        DEFAULT_GATEWAY_OBSERVATION_STALE_AFTER,
    )
    .await
}

pub async fn load_gateway_projection_input_from_nats_with_stale_after(
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
    stale_after: Duration,
) -> Result<GatewayProjectionInput, GatewaySourceError> {
    let routes = async {
        core_state
            .active_routes()
            .await
            .map_err(GatewaySourceError::from)
    };
    let observed_nodes = async {
        observations
            .node_snapshot_records()
            .await
            .map_err(GatewaySourceError::from)
    };
    let (routes, observed_nodes) = tokio::try_join!(routes, observed_nodes)?;

    Ok(gateway_projection_input_from_state(
        routes,
        observed_nodes,
        now_unix_nanos(),
        stale_after,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewaySourceError {
    Invalid { message: String },
    Unavailable { message: String },
}

impl fmt::Display for GatewaySourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { message } => write!(formatter, "invalid gateway source: {message}"),
            Self::Unavailable { message } => {
                write!(formatter, "gateway source unavailable: {message}")
            }
        }
    }
}

impl From<ActiveRouteStoreError> for GatewaySourceError {
    fn from(error: ActiveRouteStoreError) -> Self {
        match error {
            ActiveRouteStoreError::Decode(error) => Self::Invalid {
                message: format!("decode active route state: {error}"),
            },
            ActiveRouteStoreError::CorruptActiveRouteState {
                key,
                expected_target,
                actual_target,
            } => Self::Invalid {
                message: format!(
                    "active route state at {key} belongs to {actual_target:?}, not {expected_target:?}"
                ),
            },
            ActiveRouteStoreError::CorruptKey { key, actual_key } => Self::Invalid {
                message: format!(
                    "active route state key {key} does not match encoded target key {actual_key}"
                ),
            },
            error @ (ActiveRouteStoreError::Encode(_)
            | ActiveRouteStoreError::CasConflict { .. }
            | ActiveRouteStoreError::ListKeys { .. }
            | ActiveRouteStoreError::Watch { .. }
            | ActiveRouteStoreError::Get { .. }
            | ActiveRouteStoreError::Timeout { .. }) => Self::Unavailable {
                message: error.to_string(),
            },
        }
    }
}

impl From<ObservationStoreError> for GatewaySourceError {
    fn from(error: ObservationStoreError) -> Self {
        match error {
            ObservationStoreError::Decode(error) => Self::Invalid {
                message: format!("decode observation snapshot: {error}"),
            },
            ObservationStoreError::CorruptKey { key, actual_key } => Self::Invalid {
                message: format!(
                    "observation key {key} does not match observation key {actual_key}"
                ),
            },
            error @ (ObservationStoreError::OpenBucket { .. }
            | ObservationStoreError::Encode(_)
            | ObservationStoreError::ListKeys { .. }
            | ObservationStoreError::Watch { .. }
            | ObservationStoreError::Put { .. }
            | ObservationStoreError::Delete { .. }
            | ObservationStoreError::Get { .. }
            | ObservationStoreError::Timeout { .. }) => Self::Unavailable {
                message: error.to_string(),
            },
        }
    }
}

fn gateway_projection_input_from_state(
    routes: Vec<ActiveRouteState>,
    observed_nodes: Vec<NodeContainerObservationRecord>,
    now_unix_nanos: i128,
    stale_after: Duration,
) -> GatewayProjectionInput {
    GatewayProjectionInput {
        routes: routes.into_iter().map(gateway_route_from_state).collect(),
        observed_nodes: observed_nodes
            .into_iter()
            .map(|record| GatewayNodeObservation {
                freshness: observation_freshness(&record, now_unix_nanos, stale_after),
                snapshot: record.snapshot,
            })
            .collect(),
    }
}

fn gateway_route_from_state(state: ActiveRouteState) -> GatewayRoute {
    GatewayRoute {
        target: state.target,
        endpoint_port: state.endpoint_port,
        service_id: state.service_id,
        revision_id: state.revision_id,
    }
}

fn observation_freshness(
    record: &NodeContainerObservationRecord,
    now_unix_nanos: i128,
    stale_after: Duration,
) -> GatewayObservationFreshness {
    if record.observed_at_unix_nanos >= now_unix_nanos {
        return GatewayObservationFreshness::Fresh;
    }

    let age_nanos = (now_unix_nanos - record.observed_at_unix_nanos) as u128;
    if age_nanos <= stale_after.as_nanos() {
        GatewayObservationFreshness::Fresh
    } else {
        GatewayObservationFreshness::Stale
    }
}

fn now_unix_nanos() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as i128)
        .unwrap_or(0)
}
