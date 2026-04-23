use tokio::time::Instant;

use crate::model::{MachineEvent, MachineId, MachineRecord};

use super::peer::PeerState;

#[derive(Debug, Default)]
pub(crate) struct PeerStateMap {
    pub(crate) stored_peers: std::collections::HashMap<MachineId, PeerState>,
    pub(crate) transient_peers: std::collections::HashMap<MachineId, PeerState>,
}

impl PeerStateMap {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn init_from_snapshot(&mut self, records: &[MachineRecord], now: Instant) {
        let _ = now;
        for record in records {
            self.stored_peers
                .entry(record.id.clone())
                .or_insert_with(|| PeerState::from_record(record));
        }
    }

    pub(crate) fn upsert_stored(&mut self, record: &MachineRecord, now: Instant) {
        let _ = now;
        self.stored_peers
            .entry(record.id.clone())
            .and_modify(|peer_state| peer_state.update_from_record(record))
            .or_insert_with(|| PeerState::from_record(record));
        self.transient_peers.remove(&record.id);
    }

    pub(crate) fn upsert_transient(&mut self, record: &MachineRecord, now: Instant) {
        let _ = now;
        if self.stored_peers.contains_key(&record.id) {
            return;
        }

        self.transient_peers
            .entry(record.id.clone())
            .and_modify(|peer_state| peer_state.update_from_record(record))
            .or_insert_with(|| PeerState::from_record(record));
    }

    pub(crate) fn apply_event(&mut self, event: &MachineEvent, now: Instant) {
        match event {
            MachineEvent::Added(record) | MachineEvent::Updated(record) => {
                self.upsert_stored(record, now);
            }
            MachineEvent::Removed(record) => self.remove_stored(&record.id),
        }
    }

    pub(crate) fn remove_stored(&mut self, id: &MachineId) {
        self.stored_peers.remove(id);
    }

    pub(crate) fn remove_transient(&mut self, id: &MachineId) {
        self.transient_peers.remove(id);
    }
}
