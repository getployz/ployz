//! DNS projection source adapters.

use crate::fact_cache::FactCache;
use crate::intent::service::{IntentReadError, NatsIntentReader};
use crate::roles::dns::projection::{
    DnsAnswer, DnsProjectionError, DnsProjectionInput, DnsProjectionUpdate, DnsRecordSet,
};
use ployz_core::ops::RouteHostname;
use ployz_core::state::{GatewayServingStatus, MachineEndpointObservation, RouteBindingState};
use std::collections::{BTreeMap, BTreeSet};

pub async fn load_dns_projection_update_from_nats(
    intent_reader: &NatsIntentReader,
    facts: &FactCache,
) -> DnsProjectionUpdate {
    match load_dns_projection_input_from_nats(intent_reader, facts).await {
        Ok(input) => DnsProjectionUpdate::SourceAvailable(input),
        Err(DnsSourceError::Invalid { message }) => {
            DnsProjectionUpdate::SourceInvalid(DnsProjectionError::InvalidSource { message })
        }
        Err(DnsSourceError::Unavailable { .. }) => DnsProjectionUpdate::SourceUnavailable,
    }
}

pub async fn load_dns_projection_input_from_nats(
    intent_reader: &NatsIntentReader,
    facts: &FactCache,
) -> Result<DnsProjectionInput, DnsSourceError> {
    let intent = async { intent_reader.intent().await.map_err(DnsSourceError::from) };
    let intent = intent.await?;
    let gateway_statuses = facts.gateway_statuses();
    let endpoint_observations = facts.machine_endpoint_observations();

    let gateway_machine_ids = gateway_statuses
        .into_iter()
        .filter(|status| {
            matches!(
                status.serving,
                GatewayServingStatus::Current | GatewayServingStatus::LastKnownGood
            ) && status.route_count > 0
        })
        .map(|status| status.machine_id)
        .collect::<BTreeSet<_>>();
    let gateway_answers = endpoint_observations
        .into_iter()
        .filter(|observation| gateway_machine_ids.contains(&observation.machine_id))
        .collect::<Vec<_>>();

    // A serving gateway with no control endpoint is a partial discovery (its public-IP
    // echo timed out); emitting the resulting empty answer set would replace
    // last-known-good DNS with nothing. Report unavailable so the projection preserves
    // the previous usable answers until a gateway advertises an endpoint again.
    if !gateway_machine_ids.is_empty()
        && !gateway_answers
            .iter()
            .any(|observation| !observation.control_endpoints.is_empty())
    {
        return Err(DnsSourceError::Unavailable {
            message: "serving gateways have not advertised a control endpoint yet".to_owned(),
        });
    }

    Ok(dns_projection_input_from_state(
        intent.route_bindings,
        gateway_answers,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DnsSourceError {
    #[error("invalid DNS source: {message}")]
    Invalid { message: String },
    #[error("DNS source unavailable: {message}")]
    Unavailable { message: String },
}

impl From<IntentReadError> for DnsSourceError {
    fn from(error: IntentReadError) -> Self {
        Self::Unavailable {
            message: error.to_string(),
        }
    }
}

fn dns_projection_input_from_state(
    routes: Vec<RouteBindingState>,
    gateway_answers: Vec<MachineEndpointObservation>,
) -> DnsProjectionInput {
    let answers = gateway_answers
        .into_iter()
        .flat_map(|observation| observation.control_endpoints)
        .map(DnsAnswer::from_ip)
        .collect::<Vec<_>>();
    let mut records_by_hostname: BTreeMap<RouteHostname, Vec<DnsAnswer>> = BTreeMap::new();

    for route in routes {
        records_by_hostname
            .entry(route.target.hostname)
            .or_insert_with(|| answers.clone());
    }

    DnsProjectionInput {
        records: records_by_hostname
            .into_iter()
            .map(|(hostname, answers)| DnsRecordSet { hostname, answers })
            .collect(),
    }
}
