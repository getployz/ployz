use crate::mesh::WireGuardDevice;
use crate::mesh::driver::WireguardDriver;
use crate::mesh::peer_state::{PeerStateMap, sync_peers};
use crate::mesh::probe::probe_endpoints_parallel;
use crate::model::{MachineEvent, MachineId, MachineRecord};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

const PEER_SYNC_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum PeerSyncCommand {
    UpsertTransient(MachineRecord),
    RemoveTransient(MachineId),
    TickNow { done: oneshot::Sender<()> },
}

pub(crate) async fn run_peer_sync_task(
    snapshot: Vec<MachineRecord>,
    mut events: mpsc::Receiver<MachineEvent>,
    mut commands: mpsc::Receiver<PeerSyncCommand>,
    bootstrap_peers: Vec<MachineRecord>,
    network: WireguardDriver,
    local_machine_id: MachineId,
    cancel: CancellationToken,
) {
    let mut state = PeerStateMap::new();
    let now = Instant::now();
    state.init_from_snapshot(&snapshot, now);
    for record in &bootstrap_peers {
        if record.id != local_machine_id {
            state.upsert_transient(record, now);
        }
    }
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
                    PeerSyncCommand::TickNow { done } => {
                        peer_sync_tick(&mut state, &network, &local_machine_id).await;
                        let _ = done.send(());
                        continue;
                    }
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
    debug!(
        local_machine_id = %local_machine_id,
        device_peer_count = device_peers.len(),
        refreshed,
        ranked,
        "peer sync tick complete"
    );
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
    use crate::model::{MachineEvent, MachineStatus, OverlayIp, Participation, PublicKey};
    use std::net::Ipv6Addr;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

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

    #[tokio::test]
    async fn initial_sync_keeps_bootstrap_peer_until_store_catches_up() {
        let network = Arc::new(MemoryWireGuard::new());
        let driver = WireguardDriver::Memory(network.clone());
        let local_machine_id = MachineId("joiner".into());
        let snapshot = vec![test_record("joiner", PublicKey([1; 32]), vec!["self:1"])];
        let bootstrap_peers = vec![test_record(
            "founder",
            PublicKey([2; 32]),
            vec!["founder:1"],
        )];
        let (_event_tx, event_rx) = mpsc::channel::<MachineEvent>(4);
        let (_command_tx, command_rx) = mpsc::channel::<PeerSyncCommand>(4);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();

        let handle = tokio::spawn(async move {
            run_peer_sync_task(
                snapshot,
                event_rx,
                command_rx,
                bootstrap_peers,
                driver,
                local_machine_id,
                task_cancel,
            )
            .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel.cancel();
        handle.await.expect("peer sync task exits");

        let peers = network.current_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].id.0, "founder");
    }

    #[tokio::test]
    async fn peer_sync_tick_now_runs_one_pass_and_acknowledges() {
        let network = Arc::new(MemoryWireGuard::new());
        let driver = WireguardDriver::Memory(network.clone());
        let local_machine_id = MachineId("local".into());
        let snapshot = vec![test_record(
            "remote",
            PublicKey([11; 32]),
            vec!["a:1", "b:2"],
        )];
        let (_event_tx, event_rx) = mpsc::channel::<MachineEvent>(4);
        let (command_tx, command_rx) = mpsc::channel::<PeerSyncCommand>(4);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();

        let handle = tokio::spawn(async move {
            run_peer_sync_task(
                snapshot,
                event_rx,
                command_rx,
                Vec::new(),
                driver,
                local_machine_id,
                task_cancel,
            )
            .await;
        });

        let (done_tx, done_rx) = oneshot::channel();
        command_tx
            .send(PeerSyncCommand::TickNow { done: done_tx })
            .await
            .expect("send tick");
        done_rx.await.expect("tick ack");
        cancel.cancel();
        handle.await.expect("peer sync task exits");

        let peers = network.current_peers();
        assert_eq!(peers.len(), 1);
        assert_eq!(peers[0].id.0, "remote");
    }
}
