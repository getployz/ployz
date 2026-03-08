use crate::mesh::MeshNetwork;
use crate::model::{
    MachineEvent, MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
};
use ipnet::Ipv4Net;
use std::collections::HashMap;
use tracing::warn;

#[derive(Debug, Clone)]
pub(crate) struct PeerState {
    pub(crate) id: MachineId,
    pub(crate) public_key: PublicKey,
    pub(crate) overlay_ip: OverlayIp,
    pub(crate) subnet: Option<Ipv4Net>,
    pub(crate) bridge_ip: Option<OverlayIp>,
    pub(crate) endpoints: Vec<String>,
}

impl PeerState {
    pub(crate) fn from_record(record: &MachineRecord) -> Self {
        Self {
            id: record.id.clone(),
            public_key: record.public_key.clone(),
            overlay_ip: record.overlay_ip,
            subnet: record.subnet,
            bridge_ip: record.bridge_ip,
            endpoints: record.endpoints.clone(),
        }
    }

    pub(crate) fn update_from_record(&mut self, record: &MachineRecord) {
        self.public_key = record.public_key.clone();
        self.overlay_ip = record.overlay_ip;
        self.subnet = record.subnet;
        self.bridge_ip = record.bridge_ip;
        self.endpoints = record.endpoints.clone();
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

    pub(crate) fn init_from_snapshot(&mut self, records: &[MachineRecord]) {
        for r in records {
            self.stored_peers
                .entry(r.id.clone())
                .or_insert_with(|| PeerState::from_record(r));
        }
    }

    pub(crate) fn upsert_stored(&mut self, record: &MachineRecord) {
        self.stored_peers
            .entry(record.id.clone())
            .and_modify(|ps| ps.update_from_record(record))
            .or_insert_with(|| PeerState::from_record(record));
        self.transient_peers.remove(&record.id);
    }

    pub(crate) fn upsert_transient(&mut self, record: &MachineRecord) {
        if self.stored_peers.contains_key(&record.id) {
            return;
        }

        self.transient_peers
            .entry(record.id.clone())
            .and_modify(|ps| ps.update_from_record(record))
            .or_insert_with(|| PeerState::from_record(record));
    }

    pub(crate) fn apply_event(&mut self, event: &MachineEvent) {
        match event {
            MachineEvent::Added(r) | MachineEvent::Updated(r) => self.upsert_stored(r),
            MachineEvent::Removed(r) => self.remove_stored(&r.id),
        }
    }

    pub(crate) fn remove_stored(&mut self, id: &MachineId) {
        self.stored_peers.remove(id);
    }

    pub(crate) fn remove_transient(&mut self, id: &MachineId) {
        self.transient_peers.remove(id);
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
        .filter(|ps| !ps.endpoints.is_empty())
        .map(|ps| MachineRecord {
            id: ps.id.clone(),
            public_key: ps.public_key.clone(),
            overlay_ip: ps.overlay_ip,
            subnet: ps.subnet,
            bridge_ip: ps.bridge_ip,
            endpoints: ps.endpoints.clone(),
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
        })
        .collect();

    planned.extend(
        state
            .transient_peers
            .values()
            .filter(|ps| ps.id != *local_machine_id)
            .filter(|ps| !state.stored_peers.contains_key(&ps.id))
            .filter(|ps| !ps.endpoints.is_empty())
            .map(|ps| MachineRecord {
                id: ps.id.clone(),
                public_key: ps.public_key.clone(),
                overlay_ip: ps.overlay_ip,
                subnet: ps.subnet,
                bridge_ip: ps.bridge_ip,
                endpoints: ps.endpoints.clone(),
                status: MachineStatus::Unknown,
                participation: Participation::Disabled,
                last_heartbeat: 0,
                created_at: 0,
                updated_at: 0,
            }),
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
        }
    }

    #[test]
    fn plan_passes_all_endpoints() {
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
        };
        map.upsert_stored(&r);

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        assert_eq!(planned.len(), 1);
        assert_eq!(
            planned[0].endpoints,
            vec!["a:1".to_string(), "b:2".to_string(), "c:3".to_string()]
        );
    }

    #[test]
    fn plan_skips_peers_with_no_endpoints() {
        let mut map = PeerStateMap::new();
        let r = test_record("m1", vec![]);
        map.upsert_stored(&r);
        assert!(plan_mesh_peers(&map, &MachineId("local".into())).is_empty());
    }

    #[test]
    fn plan_skips_local_machine() {
        let mut map = PeerStateMap::new();
        map.upsert_stored(&test_record("local", vec!["a:1"]));
        map.upsert_stored(&test_record("remote", vec!["b:2"]));

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].id.0, "remote");
    }

    #[test]
    fn apply_event_upsert_and_remove() {
        let mut map = PeerStateMap::new();
        let r = test_record("m1", vec!["a:1"]);
        map.apply_event(&MachineEvent::Added(r));
        assert_eq!(map.stored_peers.len(), 1);

        map.apply_event(&MachineEvent::Removed(test_record("m1", vec![])));
        assert_eq!(map.stored_peers.len(), 0);
    }

    #[test]
    fn transient_peer_is_planned_before_store_publication() {
        let mut map = PeerStateMap::new();
        map.upsert_transient(&test_record("joiner", vec!["a:1"]));

        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].id.0, "joiner");
    }

    #[test]
    fn store_publication_prunes_matching_transient_peer() {
        let mut map = PeerStateMap::new();
        let transient = test_record("joiner", vec!["transient:1"]);
        let stored = test_record("joiner", vec!["stored:1"]);
        map.upsert_transient(&transient);
        map.upsert_stored(&stored);

        assert!(
            !map.transient_peers
                .contains_key(&MachineId("joiner".into()))
        );
        let planned = plan_mesh_peers(&map, &MachineId("local".into()));
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].endpoints, vec!["stored:1".to_string()]);
    }
}
