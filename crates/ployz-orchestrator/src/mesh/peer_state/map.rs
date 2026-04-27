use tokio::time::Instant;

use crate::model::{MachineEvent, MachineId, MachineObservation, MachineMembership};

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

    pub(crate) fn init_from_snapshot(&mut self, records: &[MachineMembership], now: Instant) {
        let _ = now;
        for record in records {
            self.stored_peers
                .entry(record.id.clone())
                .or_insert_with(|| PeerState::from_record(record));
        }
    }

    pub(crate) fn upsert_stored(&mut self, record: &MachineMembership, now: Instant) {
        let _ = now;
        self.stored_peers
            .entry(record.id.clone())
            .and_modify(|peer_state| peer_state.update_from_record(record))
            .or_insert_with(|| PeerState::from_record(record));
        self.transient_peers.remove(&record.id);
    }

    pub(crate) fn upsert_transient(&mut self, observation: &MachineObservation, now: Instant) {
        let _ = now;
        let id = &observation.identity.id;
        if self.stored_peers.contains_key(id) {
            return;
        }

        self.transient_peers
            .entry(id.clone())
            .and_modify(|peer_state| peer_state.update_from_observation(observation))
            .or_insert_with(|| PeerState::from_observation(observation));
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

    pub(crate) fn effective_peers<'a>(
        &'a self,
        local_machine_id: &'a MachineId,
    ) -> impl Iterator<Item = &'a PeerState> + 'a {
        self.stored_peers
            .values()
            .filter(move |peer| peer.id != *local_machine_id)
            .chain(
                self.transient_peers
                    .values()
                    .filter(move |peer| peer.id != *local_machine_id)
                    .filter(|peer| !self.stored_peers.contains_key(&peer.id)),
            )
    }
}
