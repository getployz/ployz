//! Gateway projection source adapters.

use crate::fact_cache::FactCache;
use crate::intent::service::{IntentReadError, NatsIntentReader};
use crate::roles::gateway::projection::{
    GatewayProjectionError, GatewayProjectionInput, GatewayProjectionUpdate, GatewayRoute,
    GatewayServingEntry,
};
use ployz_core::machine_runtime::MachineContainerObservationSnapshot;
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
pub async fn load_gateway_projection_update_from_nats(
    intent_reader: &NatsIntentReader,
    facts: &FactCache,
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
    facts: &FactCache,
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
        match intent.managed_lease {
            ployz_core::state::ManagedLeaseProjection::Ready { bundle, .. } => Some(bundle),
            ployz_core::state::ManagedLeaseProjection::Unacquired
            | ployz_core::state::ManagedLeaseProjection::RecordOnly { .. } => None,
        },
        intent.custom_certificates,
        intent.acme_http01_challenges,
        intent.route_bindings,
        intent.serving_target_entries,
        observed_machines,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GatewaySourceError {
    #[error("invalid gateway source: {message}")]
    Invalid { message: String },
    #[error("gateway source unavailable: {message}")]
    Unavailable { message: String },
}

impl From<IntentReadError> for GatewaySourceError {
    fn from(error: IntentReadError) -> Self {
        Self::Unavailable {
            message: error.to_string(),
        }
    }
}

fn gateway_projection_input_from_state(
    managed_cert_bundle: Option<ployz_core::cert::ManagedCertBundle>,
    custom_cert_bundles: Vec<ployz_core::cert::CustomCertBundle>,
    challenges: Vec<ployz_core::cert::AcmeHttp01Challenge>,
    routes: Vec<RouteBindingState>,
    serving: Vec<ServingTargetEntry>,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
) -> GatewayProjectionInput {
    GatewayProjectionInput {
        managed_cert_bundle,
        custom_cert_bundles,
        challenges,
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
