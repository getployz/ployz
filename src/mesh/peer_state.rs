use crate::mesh::MeshNetwork;
use crate::store::model::{MachineEvent, MachineId, MachineRecord, OverlayIp, PublicKey};
use std::collections::HashMap;
use tracing::warn;

#[derive(Debug, Clone)]
pub(crate) struct PeerState {
    pub(crate) id: MachineId,
    pub(crate) public_key: PublicKey,
    pub(crate) overlay_ip: OverlayIp,
    pub(crate) endpoints: Vec<String>,
}

impl PeerState {
    pub(crate) fn from_record(record: &MachineRecord) -> Self {
        Self {
            id: record.id.clone(),
            public_key: record.public_key.clone(),
            overlay_ip: record.overlay_ip,
            endpoints: record.endpoints.clone(),
        }
    }

    pub(crate) fn update_from_record(&mut self, record: &MachineRecord) {
        self.public_key = record.public_key.clone();
        self.overlay_ip = record.overlay_ip;
        self.endpoints = record.endpoints.clone();
    }
}

#[derive(Debug, Default)]
pub(crate) struct PeerStateMap {
    pub(crate) peers: HashMap<MachineId, PeerState>,
}

impl PeerStateMap {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn init_from_snapshot(&mut self, records: &[MachineRecord]) {
        for r in records {
            self.peers
                .entry(r.id.clone())
                .or_insert_with(|| PeerState::from_record(r));
        }
    }

    pub(crate) fn upsert(&mut self, record: &MachineRecord) {
        self.peers
            .entry(record.id.clone())
            .and_modify(|ps| ps.update_from_record(record))
            .or_insert_with(|| PeerState::from_record(record));
    }

    pub(crate) fn apply_event(&mut self, event: &MachineEvent) {
        match event {
            MachineEvent::Added(r) | MachineEvent::Updated(r) => self.upsert(r),
            MachineEvent::Removed { id } => self.remove(id),
        }
    }

    pub(crate) fn remove(&mut self, id: &MachineId) {
        self.peers.remove(id);
    }
}

pub(crate) async fn sync_peers<N: MeshNetwork>(state: &PeerStateMap, network: &N) {
    let planned = plan_mesh_peers(state);
    if let Err(e) = network.set_peers(&planned).await {
        warn!(?e, "set_peers failed");
    }
}

fn plan_mesh_peers(state: &PeerStateMap) -> Vec<MachineRecord> {
    state
        .peers
        .values()
        .filter(|ps| !ps.endpoints.is_empty())
        .map(|ps| MachineRecord {
            id: ps.id.clone(),
            public_key: ps.public_key.clone(),
            overlay_ip: ps.overlay_ip,
            endpoints: ps.endpoints.clone(),
        })
        .collect()
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
            endpoints: endpoints.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn plan_passes_all_endpoints() {
        let mut map = PeerStateMap::new();
        let r = MachineRecord {
            id: MachineId("m1".into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            endpoints: vec!["a:1".into(), "b:2".into(), "c:3".into()],
        };
        map.upsert(&r);

        let planned = plan_mesh_peers(&map);
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
        map.upsert(&r);
        assert!(plan_mesh_peers(&map).is_empty());
    }

    #[test]
    fn apply_event_upsert_and_remove() {
        let mut map = PeerStateMap::new();
        let r = test_record("m1", vec!["a:1"]);
        map.apply_event(&MachineEvent::Added(r));
        assert_eq!(map.peers.len(), 1);

        map.apply_event(&MachineEvent::Removed {
            id: MachineId("m1".into()),
        });
        assert_eq!(map.peers.len(), 0);
    }
}
