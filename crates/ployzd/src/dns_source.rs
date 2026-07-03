//! DNS projection source adapters.

use crate::dns::{
    DnsAnswer, DnsProjectionError, DnsProjectionInput, DnsProjectionUpdate, DnsRecordSet,
};
use ployz_core::ops::RouteHostname;
use ployz_core::state::{GatewayServingStatus, MachinePublicIpObservation, RouteBindingState};
use ployz_nats::core_state::{AsyncNatsCoreStateStore, RouteBindingStoreError};
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
            .route_bindings()
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
            .machine_public_ips()
            .await
            .map_err(DnsSourceError::from)
    };
    let (routes, gateway_statuses, public_ips) =
        tokio::try_join!(routes, gateway_statuses, public_ips)?;

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

impl From<RouteBindingStoreError> for DnsSourceError {
    fn from(error: RouteBindingStoreError) -> Self {
        match error {
            RouteBindingStoreError::Decode(error) => Self::Invalid {
                message: format!("decode route binding state: {error}"),
            },
            RouteBindingStoreError::CorruptActiveRouteState {
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

impl From<ObservationStoreError> for DnsSourceError {
    fn from(error: ObservationStoreError) -> Self {
        match error {
            ObservationStoreError::Decode(error) => Self::Invalid {
                message: format!("decode observation state: {error}"),
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
