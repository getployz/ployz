use crate::mesh::peer::{PEER_DOWN_INTERVAL, WireGuardPeer};
use crate::mesh::{DevicePeer, MeshNetwork};
use crate::model::{
    MachineEvent, MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
};
use ipnet::Ipv4Net;
use std::collections::HashMap;
use tokio::time::Instant;
use tracing::warn;

#[derive(Debug, Clone)]
pub(crate) struct PeerState {
    pub(crate) id: MachineId,
    pub(crate) public_key: PublicKey,
    pub(crate) overlay_ip: OverlayIp,
    pub(crate) subnet: Option<Ipv4Net>,
    pub(crate) bridge_ip: Option<OverlayIp>,
    pub(crate) runtime: WireGuardPeer,
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
        }
    }

    pub(crate) fn update_from_record(&mut self, record: &MachineRecord) {
        self.public_key = record.public_key.clone();
        self.overlay_ip = record.overlay_ip;
        self.subnet = record.subnet;
        self.bridge_ip = record.bridge_ip;
        self.runtime.update_endpoints(record.endpoints.clone());
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
    }

    fn refresh_from_device(&mut self, device_peer: Option<&DevicePeer>, now: Instant) -> bool {
        if let Some(device_peer) = device_peer {
            self.seed_from_device(device_peer, now);
            let matches_known_endpoint = device_peer.endpoint.as_deref().is_some_and(|endpoint| {
                self.runtime
                    .endpoints
                    .iter()
                    .any(|candidate| candidate == endpoint)
            });
            self.runtime.last_handshake = if matches_known_endpoint {
                device_peer.last_handshake
            } else {
                None
            };
        } else {
            self.runtime.last_handshake = None;
        }
        self.runtime.calculate_status(now);
        if self.runtime.should_change_endpoint() {
            self.runtime.rotate_endpoint(now);
            return true;
        }
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
}

pub(crate) async fn sync_peers<N: MeshNetwork>(
    state: &PeerStateMap,
    network: &N,
    local_machine_id: &MachineId,
) {
    let planned = plan_mesh_peers(state, local_machine_id);
    if let Err(e) = network.set_peers(&planned).await {
        warn!(?e, "set_peers failed");
    }
}

fn plan_mesh_peers(state: &PeerStateMap, local_machine_id: &MachineId) -> Vec<MachineRecord> {
    let mut planned: Vec<MachineRecord> = state
        .stored_peers
        .values()
        .filter(|ps| ps.id != *local_machine_id)
        .filter(|ps| !ps.runtime.endpoints.is_empty())
        .map(|ps| MachineRecord {
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
        })
        .collect();

    planned.extend(
        state
            .transient_peers
            .values()
            .filter(|ps| ps.id != *local_machine_id)
            .filter(|ps| !state.stored_peers.contains_key(&ps.id))
            .filter(|ps| !ps.runtime.endpoints.is_empty())
            .map(|ps| MachineRecord {
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
            }),
    );

    planned
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;
    use std::time::Duration;

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
        assert_eq!(planned.len(), 1);
        assert_eq!(
            planned[0].endpoints,
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
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].id.0, "remote");
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
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].endpoints, vec!["stored:1".to_string()]);
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
        assert_eq!(
            planned[0].endpoints,
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

        map.upsert_stored(
            &test_record("remote", vec!["c:3", "b:2", "d:4"]),
            now + Duration::from_secs(1),
        );

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        assert_eq!(
            planned[0].endpoints,
            vec!["b:2".to_string(), "d:4".to_string(), "c:3".to_string()]
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
        assert_eq!(
            planned[0].endpoints,
            vec!["b:2".to_string(), "a:1".to_string()]
        );
    }
}
