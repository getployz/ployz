//! DNS projection source adapters.

use crate::dns::{
    DnsAnswer, DnsProjectionError, DnsProjectionInput, DnsProjectionUpdate, DnsRecordSet,
};
use crate::intent::{IntentReadError, NatsIntentReader};
use crate::runtime_facts::RuntimeFactsCache;
use ployz_core::ops::RouteHostname;
use ployz_core::state::{GatewayServingStatus, MachinePublicIpObservation, RouteBindingState};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub async fn load_dns_projection_update_from_nats(
    intent_reader: &NatsIntentReader,
    facts: &RuntimeFactsCache,
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
    facts: &RuntimeFactsCache,
) -> Result<DnsProjectionInput, DnsSourceError> {
    let intent = async { intent_reader.intent().await.map_err(DnsSourceError::from) };
    let intent = intent.await?;
    let gateway_statuses = facts.gateway_statuses();
    let public_ips = facts.machine_public_ips();

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
    let gateway_answers = public_ips
        .into_iter()
        .filter(|observation| gateway_machine_ids.contains(&observation.machine_id))
        .collect::<Vec<_>>();

    Ok(dns_projection_input_from_state(
        intent.route_bindings,
        gateway_answers,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsSourceError {
    Invalid { message: String },
    Unavailable { message: String },
}

impl fmt::Display for DnsSourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { message } => write!(formatter, "invalid DNS source: {message}"),
            Self::Unavailable { message } => write!(formatter, "DNS source unavailable: {message}"),
        }
    }
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
    gateway_answers: Vec<MachinePublicIpObservation>,
) -> DnsProjectionInput {
    let answers = gateway_answers
        .into_iter()
        .map(|observation| DnsAnswer::from_ip(observation.public_ip))
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
