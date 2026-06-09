//! DNS projection source adapters.

use crate::dns::{
    DnsAnswer, DnsProjectionError, DnsProjectionInput, DnsProjectionUpdate, DnsRecordSet,
};
use ployz_core::ops::RouteHostname;
use ployz_core::state::{ActiveRouteState, GatewayServingStatus, NodePublicIpObservation};
use ployz_nats::core_state::{ActiveRouteReadError, AsyncNatsCoreStateStore};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub async fn load_dns_projection_update_from_nats(
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
) -> DnsProjectionUpdate {
    match load_dns_projection_input_from_nats(core_state, observations).await {
        Ok(input) => DnsProjectionUpdate::SourceAvailable(input),
        Err(DnsSourceError::Invalid { message }) => {
            DnsProjectionUpdate::SourceInvalid(DnsProjectionError::InvalidSource { message })
        }
        Err(DnsSourceError::Unavailable { .. }) => DnsProjectionUpdate::SourceUnavailable,
    }
}

pub async fn load_dns_projection_input_from_nats(
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
) -> Result<DnsProjectionInput, DnsSourceError> {
    let routes = async {
        core_state
            .active_routes()
            .await
            .map_err(DnsSourceError::from)
    };
    let gateway_statuses = async {
        observations
            .gateway_statuses()
            .await
            .map_err(DnsSourceError::from)
    };
    let public_ips = async {
        observations
            .node_public_ips()
            .await
            .map_err(DnsSourceError::from)
    };
    let (routes, gateway_statuses, public_ips) =
        tokio::try_join!(routes, gateway_statuses, public_ips)?;

    let gateway_node_ids = gateway_statuses
        .into_iter()
        .filter(|status| {
            matches!(
                status.serving,
                GatewayServingStatus::Current | GatewayServingStatus::LastKnownGood
            ) && status.route_count > 0
        })
        .map(|status| status.node_id)
        .collect::<BTreeSet<_>>();
    let gateway_answers = public_ips
        .into_iter()
        .filter(|observation| gateway_node_ids.contains(&observation.node_id))
        .collect::<Vec<_>>();

    Ok(dns_projection_input_from_state(routes, gateway_answers))
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

impl From<ActiveRouteReadError> for DnsSourceError {
    fn from(error: ActiveRouteReadError) -> Self {
        match error {
            ActiveRouteReadError::Decode(error) => Self::Invalid {
                message: format!("decode active route state: {error}"),
            },
            ActiveRouteReadError::CorruptActiveRouteState {
                key,
                expected_target,
                actual_target,
            } => Self::Invalid {
                message: format!(
                    "active route state at {key} belongs to {actual_target:?}, not {expected_target:?}"
                ),
            },
            ActiveRouteReadError::CorruptActiveRouteKey { key, actual_key } => Self::Invalid {
                message: format!(
                    "active route state key {key} does not match encoded target key {actual_key}"
                ),
            },
            error @ (ActiveRouteReadError::ListKeys { .. }
            | ActiveRouteReadError::Watch { .. }
            | ActiveRouteReadError::Get { .. }
            | ActiveRouteReadError::Timeout { .. }) => Self::Unavailable {
                message: error.to_string(),
            },
        }
    }
}

impl From<ObservationStoreError> for DnsSourceError {
    fn from(error: ObservationStoreError) -> Self {
        match error {
            ObservationStoreError::Decode(error) => Self::Invalid {
                message: format!("decode observation state: {error}"),
            },
            ObservationStoreError::CorruptNodeSnapshotKey { key, actual_key } => Self::Invalid {
                message: format!(
                    "node observation snapshot key {key} does not match snapshot key {actual_key}"
                ),
            },
            ObservationStoreError::CorruptNodePublicIpKey { key, actual_key } => Self::Invalid {
                message: format!("node public ip key {key} does not match key {actual_key}"),
            },
            ObservationStoreError::CorruptGatewayStatusKey { key, actual_key } => Self::Invalid {
                message: format!("gateway status key {key} does not match key {actual_key}"),
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

fn dns_projection_input_from_state(
    routes: Vec<ActiveRouteState>,
    gateway_answers: Vec<NodePublicIpObservation>,
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
