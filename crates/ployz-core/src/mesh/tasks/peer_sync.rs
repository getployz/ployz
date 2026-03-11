use crate::mesh::WireGuardDevice;
use crate::mesh::driver::WireguardDriver;
use crate::mesh::peer_state::{PeerStateMap, sync_peers};
use crate::mesh::probe::probe_endpoints_parallel;
use crate::model::{MachineEvent, MachineId, MachineRecord};
use tokio::sync::mpsc;
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const PEER_SYNC_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum PeerSyncCommand {
    UpsertTransient(MachineRecord),
    RemoveTransient(MachineId),
}

pub(crate) async fn run_peer_sync_task(
    snapshot: Vec<MachineRecord>,
    mut events: mpsc::Receiver<MachineEvent>,
    mut commands: mpsc::Receiver<PeerSyncCommand>,
    network: WireguardDriver,
    local_machine_id: MachineId,
    cancel: CancellationToken,
) {
    let mut state = PeerStateMap::new();
    let now = Instant::now();
    state.init_from_snapshot(&snapshot, now);
    let device_peers = read_device_peers(&network).await;
    state.seed_from_device_peers(&device_peers, now);
    rank_pending_peers(&mut state).await;
    sync_peers(&state, &network, &local_machine_id).await;
    let mut interval = tokio::time::interval(PEER_SYNC_INTERVAL);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("peer sync task cancelled");
                break;
            }
            _ = interval.tick() => {
                peer_sync_tick(&mut state, &network, &local_machine_id).await;
            }
            Some(event) = events.recv() => {
                debug!(?event, "peer sync event");
                state.apply_event(&event, Instant::now());
                rank_pending_peers(&mut state).await;
                sync_peers(&state, &network, &local_machine_id).await;
            }
            Some(command) = commands.recv() => {
                debug!(?command, "peer sync command");
                match command {
                    PeerSyncCommand::UpsertTransient(record) => {
                        state.upsert_transient(&record, Instant::now())
                    }
                    PeerSyncCommand::RemoveTransient(id) => state.remove_transient(&id),
                }
                rank_pending_peers(&mut state).await;
                sync_peers(&state, &network, &local_machine_id).await;
            }
        }
    }
}

async fn read_device_peers(network: &WireguardDriver) -> Vec<crate::mesh::DevicePeer> {
    match network.read_peers().await {
        Ok(peers) => peers,
        Err(error) => {
            warn!(?error, "failed to read wireguard peers for endpoint sync");
            Vec::new()
        }
    }
}

async fn peer_sync_tick(
    state: &mut PeerStateMap,
    network: &WireguardDriver,
    local_machine_id: &MachineId,
) {
    let now = Instant::now();
    let device_peers = read_device_peers(network).await;
    let refreshed = state.refresh_from_device_peers(&device_peers, now);
    let ranked = rank_pending_peers(state).await;
    if refreshed || ranked {
        sync_peers(state, network, local_machine_id).await;
    }
}

async fn rank_pending_peers(state: &mut PeerStateMap) -> bool {
    let now = Instant::now();
    let mut changed = false;
    for (machine_id, endpoints) in state.pending_rankings() {
        let probe_results = probe_endpoints_parallel(&endpoints).await;
        changed |= state.apply_probe_results(&machine_id, &probe_results, now);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::DevicePeer;
    use crate::mesh::driver::WireguardDriver;
    use crate::mesh::wireguard::MemoryWireGuard;
    use crate::model::{MachineStatus, OverlayIp, Participation, PublicKey};
    use std::net::Ipv6Addr;
    use std::sync::Arc;

    fn test_record(id: &str, key: PublicKey, endpoints: Vec<&str>) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: key,
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

    #[tokio::test]
    async fn peer_sync_tick_rotates_after_missing_handshake_timeout() {
        let now = Instant::now();
        let network = Arc::new(MemoryWireGuard::new());
        let driver = WireguardDriver::Memory(network.clone());
        let local_machine_id = MachineId("local".into());
        let remote_key = PublicKey([7; 32]);
        let snapshot = vec![test_record(
            "remote",
            remote_key.clone(),
            vec!["a:1", "b:2"],
        )];
        let mut state = PeerStateMap::new();
        state.init_from_snapshot(&snapshot, now - Duration::from_secs(20));
        sync_peers(&state, &driver, &local_machine_id).await;

        network.set_device_peers(vec![DevicePeer {
            public_key: remote_key,
            endpoint: Some("a:1".into()),
            last_handshake: None,
        }]);

        peer_sync_tick(&mut state, &driver, &local_machine_id).await;

        let peers = network.current_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(
            peers[0].endpoints,
            vec!["b:2".to_string(), "a:1".to_string()]
        );
    }

    #[tokio::test]
    async fn peer_sync_tick_preserves_fresh_live_endpoint_on_restart() {
        let now = Instant::now();
        let network = Arc::new(MemoryWireGuard::new());
        let driver = WireguardDriver::Memory(network.clone());
        let local_machine_id = MachineId("local".into());
        let remote_key = PublicKey([8; 32]);
        let snapshot = vec![test_record(
            "remote",
            remote_key.clone(),
            vec!["a:1", "b:2"],
        )];
        let mut state = PeerStateMap::new();
        state.init_from_snapshot(&snapshot, now);
        network.set_device_peers(vec![DevicePeer {
            public_key: remote_key,
            endpoint: Some("b:2".into()),
            last_handshake: Some(now - Duration::from_secs(5)),
        }]);
        state.seed_from_device_peers(&read_device_peers(&driver).await, now);
        sync_peers(&state, &driver, &local_machine_id).await;

        let peers = network.current_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(
            peers[0].endpoints,
            vec!["b:2".to_string(), "a:1".to_string()]
        );
    }

    #[tokio::test]
    async fn peer_sync_tick_rotates_after_stale_handshake() {
        let now = Instant::now();
        let network = Arc::new(MemoryWireGuard::new());
        let driver = WireguardDriver::Memory(network.clone());
        let local_machine_id = MachineId("local".into());
        let remote_key = PublicKey([9; 32]);
        let snapshot = vec![test_record(
            "remote",
            remote_key.clone(),
            vec!["a:1", "b:2"],
        )];
        let mut state = PeerStateMap::new();
        state.init_from_snapshot(&snapshot, now - Duration::from_secs(300));
        sync_peers(&state, &driver, &local_machine_id).await;

        network.set_device_peers(vec![DevicePeer {
            public_key: remote_key,
            endpoint: Some("a:1".into()),
            last_handshake: Some(now - Duration::from_secs(300)),
        }]);

        peer_sync_tick(&mut state, &driver, &local_machine_id).await;

        let peers = network.current_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(
            peers[0].endpoints,
            vec!["b:2".to_string(), "a:1".to_string()]
        );
    }

    #[tokio::test]
    async fn peer_sync_tick_falls_back_to_wireguard_order_when_tcp_probe_fails() {
        let now = Instant::now();
        let network = Arc::new(MemoryWireGuard::new());
        let driver = WireguardDriver::Memory(network.clone());
        let local_machine_id = MachineId("local".into());
        let remote_key = PublicKey([10; 32]);
        let snapshot = vec![test_record(
            "remote",
            remote_key.clone(),
            vec!["10.255.255.1:51820", "10.255.255.2:51820"],
        )];
        let mut state = PeerStateMap::new();
        state.init_from_snapshot(&snapshot, now - Duration::from_secs(20));
        sync_peers(&state, &driver, &local_machine_id).await;

        network.set_device_peers(vec![DevicePeer {
            public_key: remote_key,
            endpoint: Some("10.255.255.1:51820".into()),
            last_handshake: None,
        }]);

        peer_sync_tick(&mut state, &driver, &local_machine_id).await;

        let peers = network.current_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(
            peers[0].endpoints,
            vec![
                "10.255.255.2:51820".to_string(),
                "10.255.255.1:51820".to_string()
            ]
        );
    }
}
