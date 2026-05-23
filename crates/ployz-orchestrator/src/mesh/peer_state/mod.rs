mod map;
mod peer;
mod planning;

pub(crate) use map::PeerStateMap;
pub(crate) use planning::sync_peers;

#[cfg(test)]
mod tests {
    use std::net::Ipv6Addr;

    use tokio::time::Instant;

    use crate::model::{
        MachineId, MachineLifecycle, MachineMembership, MachineTopology, OverlayIp, PublicKey,
    };

    use super::map::PeerStateMap;

    fn test_record(id: &str, endpoints: Vec<&str>) -> MachineMembership {
        MachineMembership {
            id: MachineId::new(id),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            region_role: crate::model::RegionRole::HomeData,
            subnet: None,
            bridge_ip: None,
            endpoints: endpoints.into_iter().map(String::from).collect(),
            lifecycle: MachineLifecycle::Standby,
            storage_role: crate::model::StorageParticipation::default_authority().into(),
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    #[test]
    fn plan_passes_all_endpoints() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let r = MachineMembership {
            id: MachineId::new("m1"),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            region_role: crate::model::RegionRole::HomeData,
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["a:1".into(), "b:2".into(), "c:3".into()],
            lifecycle: MachineLifecycle::Standby,
            storage_role: crate::model::StorageParticipation::default_authority().into(),
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        };
        map.upsert_stored(&r, now);

        let planned = super::planning::plan_mesh_peers(
            &map,
            &MachineId::new("self"),
            &std::collections::HashMap::new(),
        );
        let [peer] = planned.as_slice() else {
            panic!("expected one planned peer");
        };
        assert_eq!(peer.endpoints, vec!["a:1", "b:2", "c:3"]);
    }

    #[test]
    fn transient_peer_is_dropped_when_stored_peer_arrives() {
        let now = Instant::now();
        let mut map = PeerStateMap::new();
        let record = test_record("m1", vec!["a:1"]);
        let observation = record.observation();
        map.upsert_transient(&observation, now);
        assert!(map.transient_peers.contains_key(&record.id));

        map.upsert_stored(&record, now);
        assert!(map.stored_peers.contains_key(&record.id));
        assert!(!map.transient_peers.contains_key(&record.id));
    }
}
