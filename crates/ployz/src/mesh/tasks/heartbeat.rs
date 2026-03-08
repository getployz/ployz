use crate::drivers::{StoreDriver, WireguardDriver};
use crate::mesh::peer::PEER_DOWN_INTERVAL;
use crate::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
use crate::model::{MachineId, MachineRecord, MachineStatus, Participation};
use crate::store::MachineStore;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const SCHEDULING_HYSTERESIS_SAMPLES: u8 = 3;

#[derive(Debug, Default)]
struct HeartbeatState {
    consecutive_good_samples: u8,
    consecutive_bad_samples: u8,
}

pub(crate) async fn run_heartbeat_task(
    machine_id: MachineId,
    store: StoreDriver,
    network: WireguardDriver,
    started: Arc<AtomicBool>,
    cancel: CancellationToken,
) {
    started.store(true, Ordering::SeqCst);
    let mut interval = tokio::time::interval(HEARTBEAT_INTERVAL);
    let mut state = HeartbeatState::default();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("heartbeat task cancelled");
                break;
            }
            _ = interval.tick() => {
                heartbeat_once(&machine_id, &store, &network, &mut state).await;
            }
        }
    }
}

async fn heartbeat_once(
    machine_id: &MachineId,
    store: &StoreDriver,
    network: &WireguardDriver,
    state: &mut HeartbeatState,
) {
    let machines = match store.list_machines().await {
        Ok(machines) => machines,
        Err(e) => {
            warn!(?e, "failed to read machines for heartbeat");
            return;
        }
    };

    let required_peers: Vec<MachineRecord> = machines
        .iter()
        .filter(|machine| machine.id != *machine_id)
        .filter(|machine| match machine.participation {
            Participation::Disabled => false,
            Participation::Enabled | Participation::Draining => true,
        })
        .cloned()
        .collect();

    let Some(mut record) = machines
        .into_iter()
        .find(|machine| machine.id == *machine_id)
    else {
        warn!("self record not found in store, skipping heartbeat");
        return;
    };

    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(err) => {
            warn!(?err, "system clock before unix epoch, skipping heartbeat");
            return;
        }
    };

    record.status = MachineStatus::Up;
    record.last_heartbeat = now;
    record.updated_at = now;

    if let Some(bridge_ip) = network.bridge_ip().await {
        record.bridge_ip = Some(bridge_ip);
    }

    let healthy_required_peers = required_peers_healthy(network, &required_peers).await;
    update_hysteresis(state, healthy_required_peers);

    match record.participation {
        Participation::Disabled => {
            if state.consecutive_good_samples >= SCHEDULING_HYSTERESIS_SAMPLES {
                record.participation = Participation::Enabled;
            }
        }
        Participation::Enabled => {
            if state.consecutive_bad_samples >= SCHEDULING_HYSTERESIS_SAMPLES {
                record.participation = Participation::Disabled;
            }
        }
        Participation::Draining => {}
    }

    if let Err(e) = store.upsert_machine(&record).await {
        warn!(?e, "heartbeat upsert failed");
    }
}

fn update_hysteresis(state: &mut HeartbeatState, healthy_required_peers: bool) {
    if healthy_required_peers {
        state.consecutive_bad_samples = 0;
        state.consecutive_good_samples = state
            .consecutive_good_samples
            .saturating_add(1)
            .min(SCHEDULING_HYSTERESIS_SAMPLES);
        return;
    }

    state.consecutive_good_samples = 0;
    state.consecutive_bad_samples = state
        .consecutive_bad_samples
        .saturating_add(1)
        .min(SCHEDULING_HYSTERESIS_SAMPLES);
}

async fn required_peers_healthy(
    network: &WireguardDriver,
    required_peers: &[MachineRecord],
) -> bool {
    if required_peers.is_empty() {
        return true;
    }

    let device_peers = match network.read_peers().await {
        Ok(peers) => peers,
        Err(e) => {
            warn!(?e, "failed to read direct wireguard peers for heartbeat");
            return false;
        }
    };

    let fresh_handshakes = fresh_handshake_map(&device_peers);
    required_peers.iter().all(|peer| {
        fresh_handshakes
            .get(&peer.public_key)
            .copied()
            .unwrap_or(false)
    })
}

fn fresh_handshake_map(device_peers: &[DevicePeer]) -> HashMap<crate::model::PublicKey, bool> {
    let now = Instant::now();
    device_peers
        .iter()
        .map(|peer| {
            let fresh = match peer.last_handshake {
                Some(last_handshake) => match now.checked_duration_since(last_handshake) {
                    Some(elapsed) => elapsed < PEER_DOWN_INTERVAL,
                    None => true,
                },
                None => false,
            };
            (peer.public_key.clone(), fresh)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::memory::{MemoryService, MemoryStore, MemoryWireGuard};
    use crate::drivers::{StoreDriver, WireguardDriver};
    use crate::mesh::DevicePeer;
    use crate::model::{MachineId, OverlayIp, PublicKey};
    use std::net::Ipv6Addr;
    use std::sync::Arc;

    #[test]
    fn update_hysteresis_tracks_good_and_bad_samples() {
        let mut state = HeartbeatState::default();
        update_hysteresis(&mut state, true);
        update_hysteresis(&mut state, true);
        assert_eq!(state.consecutive_good_samples, 2);
        assert_eq!(state.consecutive_bad_samples, 0);

        update_hysteresis(&mut state, false);
        assert_eq!(state.consecutive_good_samples, 0);
        assert_eq!(state.consecutive_bad_samples, 1);
    }

    #[test]
    fn fresh_handshake_map_marks_recent_handshakes_healthy() {
        let peer = DevicePeer {
            public_key: PublicKey([7; 32]),
            endpoint: Some("127.0.0.1:51820".into()),
            last_handshake: Some(Instant::now()),
        };

        let map = fresh_handshake_map(&[peer]);
        assert_eq!(map.get(&PublicKey([7; 32])), Some(&true));
    }

    #[test]
    fn fresh_handshake_map_marks_missing_handshakes_unhealthy() {
        let peer = DevicePeer {
            public_key: PublicKey([8; 32]),
            endpoint: None,
            last_handshake: None,
        };

        let map = fresh_handshake_map(&[peer]);
        assert_eq!(map.get(&PublicKey([8; 32])), Some(&false));
    }

    #[test]
    fn required_peer_filter_ignores_disabled_peers() {
        let machine_id = MachineId("self".into());
        let record = |id: &str, participation: Participation| MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec![],
            status: MachineStatus::Unknown,
            participation,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
        };

        let machines = [
            record("self", Participation::Disabled),
            record("disabled", Participation::Disabled),
            record("enabled", Participation::Enabled),
            record("draining", Participation::Draining),
        ];

        let required: Vec<MachineRecord> = machines
            .iter()
            .filter(|machine| machine.id != machine_id)
            .filter(|machine| match machine.participation {
                Participation::Disabled => false,
                Participation::Enabled | Participation::Draining => true,
            })
            .cloned()
            .collect();

        assert_eq!(
            required
                .iter()
                .map(|machine| machine.id.0.as_str())
                .collect::<Vec<_>>(),
            vec!["enabled", "draining"]
        );
    }

    #[tokio::test]
    async fn heartbeat_promotes_disabled_after_three_healthy_samples() {
        let (store, service, network, self_id, peer_key) =
            test_runtime(Participation::Disabled, Participation::Enabled).await;
        network.set_device_peers(vec![DevicePeer {
            public_key: peer_key,
            endpoint: Some("127.0.0.1:51820".into()),
            last_handshake: Some(Instant::now()),
        }]);

        let mut state = HeartbeatState::default();
        let store_driver = StoreDriver::Memory {
            store: store.clone(),
            service,
        };
        let network_driver = WireguardDriver::Memory(network.clone());

        for _ in 0..3 {
            heartbeat_once(&self_id, &store_driver, &network_driver, &mut state).await;
        }

        let machines = store.list_machines().await.expect("list machines");
        let self_record = machines
            .into_iter()
            .find(|machine| machine.id == self_id)
            .expect("self record");
        assert_eq!(self_record.participation, Participation::Enabled);
    }

    #[tokio::test]
    async fn heartbeat_demotes_enabled_after_three_unhealthy_samples() {
        let (store, service, network, self_id, _peer_key) =
            test_runtime(Participation::Enabled, Participation::Enabled).await;

        let mut state = HeartbeatState::default();
        let store_driver = StoreDriver::Memory {
            store: store.clone(),
            service,
        };
        let network_driver = WireguardDriver::Memory(network);

        for _ in 0..3 {
            heartbeat_once(&self_id, &store_driver, &network_driver, &mut state).await;
        }

        let machines = store.list_machines().await.expect("list machines");
        let self_record = machines
            .into_iter()
            .find(|machine| machine.id == self_id)
            .expect("self record");
        assert_eq!(self_record.participation, Participation::Disabled);
    }

    #[tokio::test]
    async fn heartbeat_ignores_disabled_joiners_in_required_set() {
        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        let network = Arc::new(MemoryWireGuard::new());
        let self_id = MachineId("self".into());
        let enabled_peer_key = PublicKey([2; 32]);

        store
            .upsert_machine(&test_machine(
                "self",
                Participation::Enabled,
                PublicKey([1; 32]),
            ))
            .await
            .expect("upsert self");
        store
            .upsert_machine(&test_machine(
                "enabled",
                Participation::Enabled,
                enabled_peer_key.clone(),
            ))
            .await
            .expect("upsert enabled");
        store
            .upsert_machine(&test_machine(
                "disabled",
                Participation::Disabled,
                PublicKey([3; 32]),
            ))
            .await
            .expect("upsert disabled");

        network.set_device_peers(vec![DevicePeer {
            public_key: enabled_peer_key,
            endpoint: Some("127.0.0.1:51820".into()),
            last_handshake: Some(Instant::now()),
        }]);

        let mut state = HeartbeatState::default();
        let store_driver = StoreDriver::Memory {
            store: store.clone(),
            service,
        };
        let network_driver = WireguardDriver::Memory(network);

        for _ in 0..3 {
            heartbeat_once(&self_id, &store_driver, &network_driver, &mut state).await;
        }

        let machines = store.list_machines().await.expect("list machines");
        let self_record = machines
            .into_iter()
            .find(|machine| machine.id == self_id)
            .expect("self record");
        assert_eq!(self_record.participation, Participation::Enabled);
    }

    #[tokio::test]
    async fn heartbeat_never_overwrites_draining() {
        let (store, service, network, self_id, peer_key) =
            test_runtime(Participation::Draining, Participation::Enabled).await;
        network.set_device_peers(vec![DevicePeer {
            public_key: peer_key,
            endpoint: Some("127.0.0.1:51820".into()),
            last_handshake: Some(Instant::now()),
        }]);

        let mut state = HeartbeatState::default();
        let store_driver = StoreDriver::Memory {
            store: store.clone(),
            service,
        };
        let network_driver = WireguardDriver::Memory(network);

        for _ in 0..3 {
            heartbeat_once(&self_id, &store_driver, &network_driver, &mut state).await;
        }

        let machines = store.list_machines().await.expect("list machines");
        let self_record = machines
            .into_iter()
            .find(|machine| machine.id == self_id)
            .expect("self record");
        assert_eq!(self_record.participation, Participation::Draining);
    }

    async fn test_runtime(
        self_participation: Participation,
        peer_participation: Participation,
    ) -> (
        Arc<MemoryStore>,
        Arc<MemoryService>,
        Arc<MemoryWireGuard>,
        MachineId,
        PublicKey,
    ) {
        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        let network = Arc::new(MemoryWireGuard::new());
        let self_id = MachineId("self".into());
        let peer_key = PublicKey([2; 32]);

        let self_record = test_machine("self", self_participation, PublicKey([1; 32]));
        let peer_record = test_machine("peer", peer_participation, peer_key.clone());
        store
            .upsert_machine(&self_record)
            .await
            .expect("upsert self");
        store
            .upsert_machine(&peer_record)
            .await
            .expect("upsert peer");

        (store, service, network, self_id, peer_key)
    }

    fn test_machine(
        id: &str,
        participation: Participation,
        public_key: PublicKey,
    ) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key,
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            status: MachineStatus::Unknown,
            participation,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
        }
    }
}
