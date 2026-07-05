//! Gateway projection source adapters.

use crate::gateway::{
    GatewayProjectionError, GatewayProjectionInput, GatewayProjectionUpdate, GatewayRoute,
    GatewayServingEntry,
};
use crate::intent::{IntentReadError, NatsIntentReader};
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use ployz_nats::observations::{
    AsyncNatsObservationStore, MachineContainerObservationRecord, ObservationStoreError,
};
use std::fmt;
pub async fn load_gateway_projection_update_from_nats(
    intent_reader: &NatsIntentReader,
    observations: &AsyncNatsObservationStore,
) -> GatewayProjectionUpdate {
    match load_gateway_projection_input_from_nats(intent_reader, observations).await {
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
    intent_reader: &NatsIntentReader,
    observations: &AsyncNatsObservationStore,
) -> Result<GatewayProjectionInput, GatewaySourceError> {
    let intent = async {
        intent_reader
            .intent()
            .await
            .map_err(GatewaySourceError::from)
    };
    let observed_machines = async {
        observations
            .machine_snapshot_records()
            .await
            .map_err(GatewaySourceError::from)
    };
    let (intent, observed_machines) = tokio::try_join!(intent, observed_machines)?;

    Ok(gateway_projection_input_from_state(
        intent.route_bindings,
        intent.serving_target_entries,
        observed_machines,
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

impl From<IntentReadError> for GatewaySourceError {
    fn from(error: IntentReadError) -> Self {
        Self::Unavailable {
            message: error.to_string(),
        }
    }
}

fn gateway_projection_input_from_state(
    routes: Vec<RouteBindingState>,
    serving: Vec<ServingTargetEntry>,
    observed_machines: Vec<MachineContainerObservationRecord>,
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
            .map(|record| record.snapshot)
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
