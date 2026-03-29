use crate::mesh::peer::{PEER_DOWN_INTERVAL, PeerStatus, WireGuardPeer};
use crate::mesh::probe::{TcpProbeResult, TcpProbeStatus};
use crate::mesh::{DevicePeer, MeshNetwork};
use crate::model::{
    MachineEvent, MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
};
use ipnet::Ipv4Net;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;
use tracing::{debug, warn};

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
    fn new(endpoint: String, advertised_index: usize) -> Self {
        Self {
            endpoint,
            advertised_index,
            last_tcp_probe_rtt: None,
            tcp_probe_status: TcpProbeStatus::Unreachable,
            last_wg_success: None,
            last_wg_failure: None,
        }
    }

    fn merge_preserving_history(&mut self, other: &Self) {
        self.last_tcp_probe_rtt = other.last_tcp_probe_rtt;
        self.tcp_probe_status = other.tcp_probe_status;
        self.last_wg_success = other.last_wg_success;
        self.last_wg_failure = other.last_wg_failure;
    }
}

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
        let previous = std::mem::take(&mut self.candidates);
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

    fn planned_endpoints(&self) -> Vec<String> {
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

    fn seed_from_device(&mut self, device_peer: &DevicePeer, now: Instant) {
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

    fn refresh_from_device(&mut self, device_peer: Option<&DevicePeer>, now: Instant) -> bool {
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

    fn apply_probe_results(
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
            .any(|candidate| candidate.tcp_probe_status == TcpProbeStatus::Reachable)
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

#[derive(Debug, Default)]
pub(crate) struct PeerStateMap {
    pub(crate) stored_peers: HashMap<MachineId, PeerState>,
    pub(crate) transient_peers: HashMap<MachineId, PeerState>,
}

impl PeerStateMap {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn init_from_snapshot(&mut self, records: &[MachineRecord], now: Instant) {
        for r in records {
            self.stored_peers
                .entry(r.id.clone())
                .or_insert_with(|| PeerState::from_record(r, now));
        }
    }

    pub(crate) fn upsert_stored(&mut self, record: &MachineRecord, now: Instant) {
        self.stored_peers
            .entry(record.id.clone())
            .and_modify(|ps| ps.update_from_record(record))
            .or_insert_with(|| PeerState::from_record(record, now));
        self.transient_peers.remove(&record.id);
    }

    pub(crate) fn upsert_transient(&mut self, record: &MachineRecord, now: Instant) {
        if self.stored_peers.contains_key(&record.id) {
            return;
        }

        self.transient_peers
            .entry(record.id.clone())
            .and_modify(|ps| ps.update_from_record(record))
            .or_insert_with(|| PeerState::from_record(record, now));
    }

    pub(crate) fn apply_event(&mut self, event: &MachineEvent, now: Instant) {
        match event {
            MachineEvent::Added(r) | MachineEvent::Updated(r) => self.upsert_stored(r, now),
            MachineEvent::Removed(r) => self.remove_stored(&r.id),
        }
    }

    pub(crate) fn remove_stored(&mut self, id: &MachineId) {
        self.stored_peers.remove(id);
    }

    pub(crate) fn remove_transient(&mut self, id: &MachineId) {
        self.transient_peers.remove(id);
    }

    pub(crate) fn seed_from_device_peers(&mut self, device_peers: &[DevicePeer], now: Instant) {
        let peers_by_key: HashMap<PublicKey, &DevicePeer> = device_peers
            .iter()
            .map(|peer| (peer.public_key.clone(), peer))
            .collect();

        for peer_state in self.stored_peers.values_mut() {
            if let Some(device_peer) = peers_by_key.get(&peer_state.public_key) {
                peer_state.seed_from_device(device_peer, now);
            }
        }
        for peer_state in self.transient_peers.values_mut() {
            if let Some(device_peer) = peers_by_key.get(&peer_state.public_key) {
                peer_state.seed_from_device(device_peer, now);
            }
        }
    }

    pub(crate) fn refresh_from_device_peers(
        &mut self,
        device_peers: &[DevicePeer],
        now: Instant,
    ) -> bool {
        let peers_by_key: HashMap<PublicKey, &DevicePeer> = device_peers
            .iter()
            .map(|peer| (peer.public_key.clone(), peer))
            .collect();

        let mut changed = false;
        for peer_state in self.stored_peers.values_mut() {
            let device_peer = peers_by_key.get(&peer_state.public_key).copied();
            changed |= peer_state.refresh_from_device(device_peer, now);
        }
        for peer_state in self.transient_peers.values_mut() {
            let device_peer = peers_by_key.get(&peer_state.public_key).copied();
            changed |= peer_state.refresh_from_device(device_peer, now);
        }
        changed
    }

    pub(crate) fn pending_rankings(&self) -> Vec<(MachineId, Vec<String>)> {
        let mut pending = Vec::new();
        for peer_state in self.stored_peers.values() {
            if peer_state.needs_ranking && peer_state.runtime.endpoints.len() > 1 {
                pending.push((peer_state.id.clone(), peer_state.runtime.endpoints.clone()));
            }
        }
        for peer_state in self.transient_peers.values() {
            if peer_state.needs_ranking
                && peer_state.runtime.endpoints.len() > 1
                && !self.stored_peers.contains_key(&peer_state.id)
            {
                pending.push((peer_state.id.clone(), peer_state.runtime.endpoints.clone()));
            }
        }
        pending
    }

    pub(crate) fn apply_probe_results(
        &mut self,
        id: &MachineId,
        results: &HashMap<String, TcpProbeResult>,
        now: Instant,
    ) -> bool {
        if let Some(peer_state) = self.stored_peers.get_mut(id) {
            return peer_state.apply_probe_results(results, now);
        }
        if let Some(peer_state) = self.transient_peers.get_mut(id) {
            return peer_state.apply_probe_results(results, now);
        }
        false
    }
}

pub(crate) async fn sync_peers<N: MeshNetwork>(
    state: &PeerStateMap,
    network: &N,
    local_machine_id: &MachineId,
) {
    let planned = plan_mesh_peers(state, local_machine_id);
    debug!(
        local_machine_id = %local_machine_id,
        peers = ?planned
            .iter()
            .map(|peer| (&peer.id, &peer.endpoints, peer.subnet))
            .collect::<Vec<_>>(),
        "peer sync applying planned wireguard peers"
    );
    if let Err(e) = network.set_peers(&planned).await {
        warn!(?e, "set_peers failed");
    }
}

fn build_candidates(
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

fn compare_candidates(a: &EndpointCandidateState, b: &EndpointCandidateState) -> Ordering {
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

fn peer_state_to_planned_record(ps: &PeerState) -> MachineRecord {
    MachineRecord {
        id: ps.id.clone(),
        public_key: ps.public_key.clone(),
        overlay_ip: ps.overlay_ip,
        subnet: ps.subnet,
        bridge_ip: ps.bridge_ip,
        endpoints: ps.planned_endpoints(),
        status: MachineStatus::Unknown,
        participation: Participation::Disabled,
        last_heartbeat: 0,
        created_at: 0,
        updated_at: 0,
        labels: std::collections::BTreeMap::new(),
    }
}

fn plan_mesh_peers(state: &PeerStateMap, local_machine_id: &MachineId) -> Vec<MachineRecord> {
    let mut planned: Vec<MachineRecord> = state
        .stored_peers
        .values()
        .filter(|ps| ps.id != *local_machine_id)
        .filter(|ps| !ps.runtime.endpoints.is_empty())
        .map(peer_state_to_planned_record)
        .collect();

    planned.extend(
        state
            .transient_peers
            .values()
            .filter(|ps| ps.id != *local_machine_id)
            .filter(|ps| !state.stored_peers.contains_key(&ps.id))
            .filter(|ps| !ps.runtime.endpoints.is_empty())
            .map(peer_state_to_planned_record),
    );

    planned
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    fn test_record(id: &str, endpoints: Vec<&str>) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: endpoints.into_iter().map(String::from).collect(),
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn plan_passes_all_endpoints() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let r = MachineRecord {
            id: MachineId("m1".into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["a:1".into(), "b:2".into(), "c:3".into()],
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        };
        map.upsert_stored(&r, now);

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        let [peer] = planned.as_slice() else {
            panic!("expected one peer");
        };
        assert_eq!(
            peer.endpoints,
            vec!["a:1".to_string(), "b:2".to_string(), "c:3".to_string()]
        );
    }

    #[test]
    fn plan_skips_peers_with_no_endpoints() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let r = test_record("m1", vec![]);
        map.upsert_stored(&r, now);
        assert!(plan_mesh_peers(&map, &MachineId("local".into())).is_empty());
    }

    #[test]
    fn plan_skips_local_machine() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        map.upsert_stored(&test_record("local", vec!["a:1"]), now);
        map.upsert_stored(&test_record("remote", vec!["b:2"]), now);

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        let [peer] = planned.as_slice() else {
            panic!("expected one peer");
        };
        assert_eq!(peer.id.0, "remote");
    }

    #[test]
    fn apply_event_upsert_and_remove() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let r = test_record("m1", vec!["a:1"]);
        map.apply_event(&MachineEvent::Added(r), now);
        assert_eq!(map.stored_peers.len(), 1);

        map.apply_event(&MachineEvent::Removed(test_record("m1", vec![])), now);
        assert_eq!(map.stored_peers.len(), 0);
    }

    #[test]
    fn transient_peer_is_planned_before_store_publication() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        map.upsert_transient(&test_record("joiner", vec!["a:1"]), now);

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].id.0, "joiner");
    }

    #[test]
    fn store_publication_prunes_matching_transient_peer() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let transient = test_record("joiner", vec!["transient:1"]);
        let stored = test_record("joiner", vec!["stored:1"]);
        map.upsert_transient(&transient, now);
        map.upsert_stored(&stored, now);

        assert!(
            !map.transient_peers
                .contains_key(&MachineId("joiner".into()))
        );
        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        let [peer] = planned.as_slice() else {
            panic!("expected one peer");
        };
        assert_eq!(peer.endpoints, vec!["stored:1".to_string()]);
    }

    #[test]
    fn plan_puts_active_endpoint_first() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let remote_id = MachineId("remote".into());
        map.upsert_stored(&test_record("remote", vec!["a:1", "b:2", "c:3"]), now);
        let peer_state = map
            .stored_peers
            .get_mut(&remote_id)
            .expect("peer state for remote");
        peer_state.runtime.active_endpoint = 1;

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        let [peer] = planned.as_slice() else {
            panic!("expected one peer");
        };
        assert_eq!(
            peer.endpoints,
            vec!["b:2".to_string(), "c:3".to_string(), "a:1".to_string()]
        );
    }

    #[test]
    fn update_preserves_active_endpoint_across_record_changes() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let remote_id = MachineId("remote".into());
        map.upsert_stored(&test_record("remote", vec!["a:1", "b:2", "c:3"]), now);
        let peer_state = map
            .stored_peers
            .get_mut(&remote_id)
            .expect("peer state for remote");
        peer_state.runtime.active_endpoint = 1;
        peer_state.runtime.status = PeerStatus::Up;

        map.upsert_stored(
            &test_record("remote", vec!["c:3", "b:2", "d:4"]),
            now + Duration::from_secs(1),
        );

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        let [peer] = planned.as_slice() else {
            panic!("expected one peer");
        };
        assert_eq!(
            peer.endpoints,
            vec!["b:2".to_string(), "d:4".to_string(), "c:3".to_string()]
        );
        assert!(
            !map.stored_peers
                .get(&remote_id)
                .expect("peer state for remote")
                .needs_ranking
        );
    }

    #[test]
    fn update_marks_peer_for_ranking_when_active_endpoint_is_removed() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let remote_id = MachineId("remote".into());
        map.upsert_stored(&test_record("remote", vec!["a:1", "b:2", "c:3"]), now);
        let peer_state = map
            .stored_peers
            .get_mut(&remote_id)
            .expect("peer state for remote");
        peer_state.runtime.active_endpoint = 1;
        peer_state.runtime.status = PeerStatus::Up;

        map.upsert_stored(
            &test_record("remote", vec!["c:3", "d:4"]),
            now + Duration::from_secs(1),
        );

        assert!(
            map.stored_peers
                .get(&remote_id)
                .expect("peer state for remote")
                .needs_ranking
        );
    }

    #[test]
    fn seed_from_live_device_preserves_connected_endpoint() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let remote_key = PublicKey([3; 32]);
        map.upsert_stored(
            &MachineRecord {
                public_key: remote_key.clone(),
                ..test_record("remote", vec!["a:1", "b:2"])
            },
            now,
        );
        map.seed_from_device_peers(
            &[DevicePeer {
                public_key: remote_key,
                endpoint: Some("b:2".into()),
                last_handshake: Some(now - Duration::from_secs(5)),
            }],
            now,
        );

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        let [peer] = planned.as_slice() else {
            panic!("expected one peer");
        };
        assert_eq!(peer.endpoints, vec!["b:2".to_string(), "a:1".to_string()]);
    }

    #[test]
    fn apply_probe_results_prefers_reachable_then_low_rtt() {
        let now = Instant::now();
        let mut peer_state =
            PeerState::from_record(&test_record("remote", vec!["a:1", "b:2", "c:3"]), now);
        let results = HashMap::from([
            (
                "a:1".to_string(),
                TcpProbeResult {
                    status: TcpProbeStatus::Unreachable,
                    rtt: None,
                },
            ),
            (
                "b:2".to_string(),
                TcpProbeResult {
                    status: TcpProbeStatus::Reachable,
                    rtt: Some(Duration::from_millis(30)),
                },
            ),
            (
                "c:3".to_string(),
                TcpProbeResult {
                    status: TcpProbeStatus::Reachable,
                    rtt: Some(Duration::from_millis(10)),
                },
            ),
        ]);

        assert!(peer_state.apply_probe_results(&results, now + Duration::from_secs(1)));
        let Some(endpoint) = peer_state.runtime.endpoints.first() else {
            panic!("expected one endpoint");
        };
        assert_eq!(endpoint, "c:3");
        assert_eq!(
            peer_state.selection_reason,
            SelectionReason::TcpProbeRanking
        );
    }

    #[test]
    fn apply_probe_results_keeps_existing_order_when_no_probe_succeeds() {
        let now = Instant::now();
        let mut peer_state =
            PeerState::from_record(&test_record("remote", vec!["a:1", "b:2"]), now);
        let results = HashMap::from([
            (
                "a:1".to_string(),
                TcpProbeResult {
                    status: TcpProbeStatus::Unreachable,
                    rtt: None,
                },
            ),
            (
                "b:2".to_string(),
                TcpProbeResult {
                    status: TcpProbeStatus::Unreachable,
                    rtt: None,
                },
            ),
        ]);

        assert!(!peer_state.apply_probe_results(&results, now + Duration::from_secs(1)));
        assert_eq!(peer_state.runtime.active_endpoint(), Some("a:1"));
        assert_eq!(
            peer_state.selection_reason,
            SelectionReason::AdvertisedOrder
        );
    }
}
