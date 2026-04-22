use crate::mesh::probe::TcpProbeStatus;
use std::cmp::Ordering;
use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SelectionReason {
    AdvertisedOrder,
    PreservedLiveEndpoint,
    TcpProbeRanking,
    WireguardFallback,
}

#[derive(Debug, Clone)]
pub(crate) struct EndpointCandidateState {
    pub(crate) endpoint: String,
    pub(crate) advertised_index: usize,
    pub(crate) last_tcp_probe_rtt: Option<Duration>,
    pub(crate) tcp_probe_status: TcpProbeStatus,
    pub(crate) last_wg_success: Option<Instant>,
    pub(crate) last_wg_failure: Option<Instant>,
}

impl EndpointCandidateState {
    pub(super) fn new(endpoint: String, advertised_index: usize) -> Self {
        Self {
            endpoint,
            advertised_index,
            last_tcp_probe_rtt: None,
            tcp_probe_status: TcpProbeStatus::Unreachable,
            last_wg_success: None,
            last_wg_failure: None,
        }
    }

    pub(super) fn merge_preserving_history(&mut self, other: &Self) {
        self.last_tcp_probe_rtt = other.last_tcp_probe_rtt;
        self.tcp_probe_status = other.tcp_probe_status;
        self.last_wg_success = other.last_wg_success;
        self.last_wg_failure = other.last_wg_failure;
    }
}

pub(super) fn build_candidates(
    endpoints: &[String],
    existing: &[EndpointCandidateState],
) -> Vec<EndpointCandidateState> {
    endpoints
        .iter()
        .enumerate()
        .map(|(index, endpoint)| {
            let mut candidate = EndpointCandidateState::new(endpoint.clone(), index);
            if let Some(previous) = existing
                .iter()
                .find(|previous| previous.endpoint == *endpoint)
            {
                candidate.merge_preserving_history(previous);
            }
            candidate
        })
        .collect()
}

pub(super) fn compare_candidates(a: &EndpointCandidateState, b: &EndpointCandidateState) -> Ordering {
    match (a.tcp_probe_status, b.tcp_probe_status) {
        (TcpProbeStatus::Reachable, TcpProbeStatus::Unreachable) => return Ordering::Less,
        (TcpProbeStatus::Unreachable, TcpProbeStatus::Reachable) => return Ordering::Greater,
        (TcpProbeStatus::Reachable, TcpProbeStatus::Reachable)
        | (TcpProbeStatus::Unreachable, TcpProbeStatus::Unreachable) => {}
    }

    match compare_option_duration_asc(a.last_tcp_probe_rtt, b.last_tcp_probe_rtt) {
        Ordering::Equal => {}
        ordering @ (Ordering::Less | Ordering::Greater) => return ordering,
    }

    match compare_option_instant_desc(a.last_wg_success, b.last_wg_success) {
        Ordering::Equal => {}
        ordering @ (Ordering::Less | Ordering::Greater) => return ordering,
    }

    a.advertised_index.cmp(&b.advertised_index)
}

fn compare_option_duration_asc(a: Option<Duration>, b: Option<Duration>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn compare_option_instant_desc(a: Option<Instant>, b: Option<Instant>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
