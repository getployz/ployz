use std::collections::HashMap;

use ipnet::Ipv4Net;
use tokio::time::Instant;
use tracing::debug;

use crate::mesh::peer::{PEER_DOWN_INTERVAL, PeerStatus, WireGuardPeer};
use crate::mesh::probe::TcpProbeResult;
use crate::mesh::DevicePeer;
use crate::model::{MachineId, MachineRecord, OverlayIp, PublicKey};

use super::candidate::{
    EndpointCandidateState, SelectionReason, build_candidates, compare_candidates,
};

#[derive(Debug, Clone)]
pub(crate) struct PeerState {
    pub(crate) id: MachineId,
    pub(crate) public_key: PublicKey,
    pub(crate) overlay_ip: OverlayIp,
    pub(crate) subnet: Option<Ipv4Net>,
    pub(crate) bridge_ip: Option<OverlayIp>,
    pub(crate) runtime: WireGuardPeer,
    pub(crate) candidates: Vec<EndpointCandidateState>,
    pub(crate) selection_reason: SelectionReason,
    pub(crate) needs_ranking: bool,
}

impl PeerState {
    pub(crate) fn from_record(record: &MachineRecord, now: Instant) -> Self {
        Self {
            id: record.id.clone(),
            public_key: record.public_key.clone(),
            overlay_ip: record.overlay_ip,
            subnet: record.subnet,
            bridge_ip: record.bridge_ip,
            runtime: WireGuardPeer::new(record.endpoints.clone(), now),
            candidates: build_candidates(&record.endpoints, &[]),
            selection_reason: SelectionReason::AdvertisedOrder,
            needs_ranking: !record.endpoints.is_empty(),
        }
    }

    pub(crate) fn update_from_record(&mut self, record: &MachineRecord) {
        let previous_active = self.active_endpoint_value().map(str::to_string);
        self.public_key = record.public_key.clone();
        self.overlay_ip = record.overlay_ip;
        self.subnet = record.subnet;
        self.bridge_ip = record.bridge_ip;
        let previous = self.candidates.clone();
        self.candidates = build_candidates(&record.endpoints, &previous);
        self.runtime.update_endpoints(record.endpoints.clone());
        if self.runtime.endpoints.is_empty() {
            self.needs_ranking = false;
            return;
        }

        let active_removed = previous_active.as_ref().is_some_and(|endpoint| {
            !self
                .runtime
                .endpoints
                .iter()
                .any(|candidate| candidate == endpoint)
        });

        self.needs_ranking = match self.runtime.status {
            PeerStatus::Up if !active_removed => false,
            PeerStatus::Up | PeerStatus::Down | PeerStatus::Unknown => true,
        };
    }

    pub(super) fn planned_endpoints(&self) -> Vec<String> {
        let mut endpoints = self.runtime.endpoints.clone();
        if endpoints.is_empty() {
            return endpoints;
        }
        let active_endpoint = self.runtime.active_endpoint % endpoints.len();
        endpoints.rotate_left(active_endpoint);
        endpoints
    }

    fn active_endpoint_value(&self) -> Option<&str> {
        self.runtime.active_endpoint()
    }

    pub(super) fn seed_from_device(&mut self, device_peer: &DevicePeer, now: Instant) {
        let Some(endpoint) = device_peer.endpoint.as_deref() else {
            return;
        };
        let Some(last_handshake) = device_peer.last_handshake else {
            return;
        };
        let Some(elapsed) = now.checked_duration_since(last_handshake) else {
            return;
        };
        if elapsed >= PEER_DOWN_INTERVAL {
            return;
        }
        let Some(active_endpoint) = self
            .runtime
            .endpoints
            .iter()
            .position(|candidate| candidate == endpoint)
        else {
            return;
        };

        self.runtime.active_endpoint = active_endpoint;
        self.runtime.last_endpoint_change = last_handshake;
        self.runtime.last_handshake = Some(last_handshake);
        self.runtime.calculate_status(now);
        if let Some(candidate) = self
            .candidates
            .iter_mut()
            .find(|candidate| candidate.endpoint == endpoint)
        {
            candidate.last_wg_success = Some(last_handshake);
        }
        self.selection_reason = SelectionReason::PreservedLiveEndpoint;
        self.needs_ranking = false;
    }

    pub(super) fn refresh_from_device(
        &mut self,
        device_peer: Option<&DevicePeer>,
        now: Instant,
    ) -> bool {
        if let Some(device_peer) = device_peer {
            let configured_endpoint = self.active_endpoint_value().map(str::to_string);
            let device_endpoint = device_peer.endpoint.clone();
            self.seed_from_device(device_peer, now);
            let matches_known_endpoint = device_peer.endpoint.as_deref().is_some_and(|endpoint| {
                self.runtime
                    .endpoints
                    .iter()
                    .any(|candidate| candidate == endpoint)
            });
            if device_endpoint.as_deref() != configured_endpoint.as_deref() {
                debug!(
                    machine_id = %self.id,
                    ?configured_endpoint,
                    ?device_endpoint,
                    matches_known_endpoint,
                    "peer sync observed device endpoint differing from configured active endpoint"
                );
            }
            self.runtime.last_handshake = if matches_known_endpoint {
                device_peer.last_handshake
            } else {
                None
            };
            if let Some(endpoint) = device_peer.endpoint.as_deref()
                && let Some(last_handshake) = device_peer.last_handshake
                && let Some(candidate) = self
                    .candidates
                    .iter_mut()
                    .find(|candidate| candidate.endpoint == endpoint)
            {
                candidate.last_wg_success = Some(last_handshake);
            }
        } else {
            debug!(machine_id = %self.id, "peer sync found no device peer for configured machine");
            self.runtime.last_handshake = None;
        }

        self.runtime.calculate_status(now);
        if self.runtime.status == PeerStatus::Up {
            self.needs_ranking = false;
            return false;
        }

        if self.runtime.status == PeerStatus::Down && self.runtime.endpoints.len() > 1 {
            let previous_active = self.active_endpoint_value().map(str::to_string);
            if let Some(active_endpoint) = previous_active.as_ref()
                && let Some(candidate) = self
                    .candidates
                    .iter_mut()
                    .find(|candidate| candidate.endpoint == *active_endpoint)
            {
                candidate.last_wg_failure = Some(now);
            }

            self.runtime.rotate_endpoint(now);
            self.selection_reason = SelectionReason::WireguardFallback;
            self.needs_ranking = false;
            debug!(
                machine_id = %self.id,
                ?previous_active,
                next_active = ?self.active_endpoint_value(),
                status = ?self.runtime.status,
                "peer sync rotated endpoint after stale or missing handshake"
            );
            return previous_active.as_deref() != self.runtime.active_endpoint();
        }

        self.needs_ranking = false;
        false
    }

    pub(super) fn apply_probe_results(
        &mut self,
        results: &HashMap<String, TcpProbeResult>,
        now: Instant,
    ) -> bool {
        for candidate in &mut self.candidates {
            if let Some(result) = results.get(&candidate.endpoint) {
                candidate.tcp_probe_status = result.status;
                candidate.last_tcp_probe_rtt = result.rtt;
            }
        }

        if self
            .candidates
            .iter()
            .any(|candidate| candidate.tcp_probe_status == crate::mesh::probe::TcpProbeStatus::Reachable)
        {
            let previous_active = self.runtime.active_endpoint().map(str::to_string);
            self.candidates.sort_by(compare_candidates);
            let ranked_endpoints: Vec<String> = self
                .candidates
                .iter()
                .map(|candidate| candidate.endpoint.clone())
                .collect();
            self.runtime.endpoints = ranked_endpoints;
            self.runtime.active_endpoint = 0;
            self.runtime.last_endpoint_change = now;
            self.runtime.last_handshake = None;
            self.runtime.status = PeerStatus::Unknown;
            self.selection_reason = SelectionReason::TcpProbeRanking;
            self.needs_ranking = false;
            debug!(
                machine_id = %self.id,
                ?previous_active,
                next_active = ?self.runtime.active_endpoint(),
                endpoints = ?self.runtime.endpoints,
                "peer sync re-ranked endpoints from TCP probe results"
            );
            return previous_active.as_deref() != self.runtime.active_endpoint();
        }

        self.needs_ranking = false;
        false
    }
}
