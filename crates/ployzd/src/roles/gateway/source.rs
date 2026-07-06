//! Gateway projection source adapters.

use crate::roles::gateway::projection::{
    GatewayProjectionError, GatewayProjectionInput, GatewayProjectionUpdate, GatewayRoute,
    GatewayServingEntry,
};
use crate::intent::{IntentReadError, NatsIntentReader};
use crate::fact_cache::RuntimeFactsCache;
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use std::fmt;
pub async fn load_gateway_projection_update_from_nats(
    intent_reader: &NatsIntentReader,
    facts: &RuntimeFactsCache,
) -> GatewayProjectionUpdate {
    match load_gateway_projection_input_from_nats(intent_reader, facts).await {
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
    facts: &RuntimeFactsCache,
) -> Result<GatewayProjectionInput, GatewaySourceError> {
    let intent = async {
        intent_reader
            .intent()
            .await
            .map_err(GatewaySourceError::from)
    };
    let intent = intent.await?;
    let observed_machines = facts.machine_container_snapshots();

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
    observed_machines: Vec<MachineContainerObservationSnapshot>,
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
        observed_machines,
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
