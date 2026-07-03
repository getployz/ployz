//! Gateway projection source adapters.

use crate::gateway::{
    GatewayMachineObservation, GatewayObservationFreshness, GatewayProjectionError,
    GatewayProjectionInput, GatewayProjectionUpdate, GatewayRoute, GatewayServingEntry,
};
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use ployz_nats::core_state::{
    AsyncNatsCoreStateStore, CoreStateStoreError, RouteBindingStoreError,
};
use ployz_nats::observations::{
    AsyncNatsObservationStore, MachineContainerObservationRecord, ObservationStoreError,
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
            .route_bindings()
            .await
            .map_err(GatewaySourceError::from)
    };
    let serving = async {
        core_state
            .serving_target_entries()
            .await
            .map_err(GatewaySourceError::from)
    };
    let observed_machines = async {
        observations
            .machine_snapshot_records()
            .await
            .map_err(GatewaySourceError::from)
    };
    let (routes, serving, observed_machines) =
        tokio::try_join!(routes, serving, observed_machines)?;

    Ok(gateway_projection_input_from_state(
        routes,
        serving,
        observed_machines,
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

impl From<RouteBindingStoreError> for GatewaySourceError {
    fn from(error: RouteBindingStoreError) -> Self {
        match error {
            RouteBindingStoreError::Decode(error) => Self::Invalid {
                message: format!("decode route binding state: {error}"),
            },
            RouteBindingStoreError::CorruptRouteBindingState {
                key,
                expected_target,
                actual_target,
            } => Self::Invalid {
                message: format!(
                    "route binding state at {key} belongs to {actual_target:?}, not {expected_target:?}"
                ),
            },
            RouteBindingStoreError::CorruptKey { key, actual_key } => Self::Invalid {
                message: format!(
                    "route binding state key {key} does not match encoded target key {actual_key}"
                ),
            },
            error @ (RouteBindingStoreError::Encode(_)
            | RouteBindingStoreError::Put { .. }
            | RouteBindingStoreError::Delete { .. }
            | RouteBindingStoreError::ListKeys { .. }
            | RouteBindingStoreError::Watch { .. }
            | RouteBindingStoreError::Get { .. }
            | RouteBindingStoreError::Timeout { .. }) => Self::Unavailable {
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
    routes: Vec<RouteBindingState>,
    serving: Vec<ServingTargetEntry>,
    observed_machines: Vec<MachineContainerObservationRecord>,
    now_unix_nanos: i128,
    stale_after: Duration,
) -> GatewayProjectionInput {
    GatewayProjectionInput {
        routes: routes.into_iter().map(gateway_route_from_state).collect(),
        serving: serving
            .into_iter()
            .map(|state| GatewayServingEntry {
                namespace_id: state.namespace_id,
                service_id: state.service_id,
                namespace_revision_entry_id: state.namespace_revision_entry_id,
            })
            .collect(),
        observed_machines: observed_machines
            .into_iter()
            .map(|record| GatewayMachineObservation {
                freshness: observation_freshness(&record, now_unix_nanos, stale_after),
                snapshot: record.snapshot,
            })
            .collect(),
    }
}

fn gateway_route_from_state(state: RouteBindingState) -> GatewayRoute {
    GatewayRoute {
        target: state.target,
        endpoint_port: state.endpoint_port,
        namespace_id: state.namespace_id,
        service_id: state.service_id,
    }
}

impl From<CoreStateStoreError> for GatewaySourceError {
    fn from(error: CoreStateStoreError) -> Self {
        match error {
            CoreStateStoreError::Decode(error) => Self::Invalid {
                message: format!("decode serving target entry state: {error}"),
            },
            CoreStateStoreError::CorruptServingTargetEntry {
                key,
                expected_service_id,
                actual_service_id,
            } => Self::Invalid {
                message: format!(
                    "serving target entry state at {key} belongs to {actual_service_id:?}, not {expected_service_id:?}"
                ),
            },
            CoreStateStoreError::CorruptKey { key, actual_key } => Self::Invalid {
                message: format!(
                    "serving target entry state key {key} does not match encoded key {actual_key}"
                ),
            },
            error @ (CoreStateStoreError::OpenBucket { .. }
            | CoreStateStoreError::Encode(_)
            | CoreStateStoreError::Put { .. }
            | CoreStateStoreError::CasConflict { .. }
            | CoreStateStoreError::Get { .. }
            | CoreStateStoreError::Delete { .. }
            | CoreStateStoreError::ListKeys { .. }
            | CoreStateStoreError::CorruptNamespaceLock { .. }
            | CoreStateStoreError::Timeout { .. }) => Self::Unavailable {
                message: error.to_string(),
            },
        }
    }
}

fn observation_freshness(
    record: &MachineContainerObservationRecord,
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
