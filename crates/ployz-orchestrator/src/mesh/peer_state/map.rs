use std::collections::HashMap;

use tokio::time::Instant;

use crate::mesh::probe::TcpProbeResult;
use crate::mesh::DevicePeer;
use crate::model::{MachineEvent, MachineId, MachineRecord, PublicKey};

use super::peer::PeerState;

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
