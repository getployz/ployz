use super::reconcile::PeerHealth;
use crate::domain::model::{
    MachineId, MachineRecord, NetworkId, NetworkName, OverlayIp, PublicKey,
};
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub(crate) struct PeerState {
    pub(crate) id: MachineId,
    pub(crate) network_id: NetworkId,
    pub(crate) network: NetworkName,
    pub(crate) public_key: PublicKey,
    pub(crate) overlay_ip: OverlayIp,
    pub(crate) endpoints: Vec<String>,
    pub(crate) active_endpoint: usize,
    pub(crate) attempted: usize,
    pub(crate) last_handshake: Option<Instant>,
    pub(crate) health: PeerHealth,
}

impl PeerState {
    pub(crate) fn from_record(record: &MachineRecord) -> Self {
        Self {
            id: record.id.clone(),
            network_id: record.network_id.clone(),
            network: record.network.clone(),
            public_key: record.public_key.clone(),
            overlay_ip: record.overlay_ip,
            endpoints: record.endpoints.clone(),
            active_endpoint: 0,
            attempted: 0,
            last_handshake: None,
            health: PeerHealth::New,
        }
    }

    pub(crate) fn update_from_record(&mut self, record: &MachineRecord) {
        self.network_id = record.network_id.clone();
        self.network = record.network.clone();
        self.public_key = record.public_key.clone();
        self.overlay_ip = record.overlay_ip;
        self.endpoints = record.endpoints.clone();
        if !self.endpoints.is_empty() {
            self.active_endpoint = self.active_endpoint.min(self.endpoints.len() - 1);
        }
    }

    pub(crate) fn classify(&mut self, now: Instant, handshake_timeout: Duration) {
        match self.last_handshake {
            Some(t) if now.duration_since(t) < handshake_timeout => {
                self.health = PeerHealth::Alive;
                self.attempted = 0;
            }
            Some(_) => self.health = PeerHealth::Suspect,
            None if self.health == PeerHealth::New => {}
            None => self.health = PeerHealth::Suspect,
        }
    }

    pub(crate) fn should_rotate(&self, now: Instant, rotation_timeout: Duration) -> bool {
        if self.endpoints.len() <= 1 {
            return false;
        }
        match self.last_handshake {
            Some(t) => now.duration_since(t) >= rotation_timeout,
            None => self.health == PeerHealth::Suspect,
        }
    }

    pub(crate) fn next_endpoint(&mut self) {
        if self.endpoints.is_empty() {
            return;
        }
        self.active_endpoint = (self.active_endpoint + 1) % self.endpoints.len();
        self.attempted += 1;
    }

    pub(crate) fn current_endpoint(&self) -> Option<&str> {
        self.endpoints.get(self.active_endpoint).map(|s| s.as_str())
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

    pub(crate) fn remove(&mut self, id: &MachineId) {
        self.peers.remove(id);
    }

    pub(crate) fn apply_handshakes(&mut self, handshakes: &HashMap<PublicKey, Option<Instant>>) {
        for ps in self.peers.values_mut() {
            if let Some(hs) = handshakes.get(&ps.public_key) {
                ps.last_handshake = *hs;
            }
        }
    }
}

pub(crate) fn plan_mesh_peers(state: &PeerStateMap, network_id: &NetworkId) -> Vec<MachineRecord> {
    state
        .peers
        .values()
        .filter(|ps| ps.network_id == *network_id && !ps.endpoints.is_empty())
        .map(|ps| {
            let active_ep = ps.current_endpoint().unwrap_or_default().to_string();
            MachineRecord {
                id: ps.id.clone(),
                network_id: ps.network_id.clone(),
                network: ps.network.clone(),
                public_key: ps.public_key.clone(),
                overlay_ip: ps.overlay_ip,
                endpoints: vec![active_ep],
            }
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
            network_id: NetworkId("test-net".into()),
            network: NetworkName("test".into()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            endpoints: endpoints.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn classify_alive_on_fresh_handshake() {
        let mut ps = PeerState::from_record(&test_record("m1", vec!["1.2.3.4:51820"]));
        let now = Instant::now();
        ps.last_handshake = Some(now);
        ps.classify(now, Duration::from_secs(15));
        assert_eq!(ps.health, PeerHealth::Alive);
    }

    #[test]
    fn classify_suspect_on_stale_handshake() {
        let mut ps = PeerState::from_record(&test_record("m1", vec!["1.2.3.4:51820"]));
        let past = Instant::now();
        ps.last_handshake = Some(past);
        ps.classify(past + Duration::from_secs(20), Duration::from_secs(15));
        assert_eq!(ps.health, PeerHealth::Suspect);
    }

    #[test]
    fn no_rotation_single_endpoint() {
        let ps = PeerState::from_record(&test_record("m1", vec!["1.2.3.4:51820"]));
        assert!(!ps.should_rotate(
            Instant::now() + Duration::from_secs(100),
            Duration::from_secs(15)
        ));
    }

    #[test]
    fn rotation_wraps_around() {
        let mut ps = PeerState::from_record(&test_record("m1", vec!["a:1", "b:2", "c:3"]));
        assert_eq!(ps.active_endpoint, 0);
        ps.next_endpoint();
        assert_eq!(ps.active_endpoint, 1);
        ps.next_endpoint();
        assert_eq!(ps.active_endpoint, 2);
        ps.next_endpoint();
        assert_eq!(ps.active_endpoint, 0);
    }

    #[test]
    fn plan_uses_active_endpoint() {
        let mut map = PeerStateMap::new();
        let r = MachineRecord {
            id: MachineId("m1".into()),
            network_id: NetworkId("test-net".into()),
            network: NetworkName("test".into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            endpoints: vec!["a:1".into(), "b:2".into(), "c:3".into()],
        };
        let mut ps = PeerState::from_record(&r);
        ps.next_endpoint();
        map.peers.insert(r.id.clone(), ps);

        let planned = plan_mesh_peers(&map, &NetworkId("test-net".into()));
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].endpoints, vec!["b:2".to_string()]);
    }

    #[test]
    fn plan_skips_peers_with_no_endpoints() {
        let mut map = PeerStateMap::new();
        let r = test_record("m1", vec![]);
        map.peers.insert(r.id.clone(), PeerState::from_record(&r));
        assert!(plan_mesh_peers(&map, &NetworkId("test-net".into())).is_empty());
    }

    #[test]
    fn plan_filters_other_networks() {
        let mut map = PeerStateMap::new();
        let mut a = test_record("m1", vec!["a:1"]);
        a.network_id = NetworkId("net-a".into());
        map.peers.insert(a.id.clone(), PeerState::from_record(&a));

        let mut b = test_record("m2", vec!["b:2"]);
        b.network_id = NetworkId("net-b".into());
        map.peers.insert(b.id.clone(), PeerState::from_record(&b));

        let planned = plan_mesh_peers(&map, &NetworkId("net-a".into()));
        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].id.0, "m1");
    }
}
