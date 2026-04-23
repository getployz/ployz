use crate::mesh::driver::WireguardDriver;
use crate::mesh::peer_state::{PeerStateMap, sync_peers};
use crate::model::{MachineEvent, MachineId, MachineRecord};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

#[derive(Debug)]
pub enum PeerSyncCommand {
    UpsertTransient(MachineRecord),
    RemoveTransient(MachineId),
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
    sync_peers(&state, &network, &local_machine_id).await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("peer sync task cancelled");
                break;
            }
            Some(event) = events.recv() => {
                debug!(?event, "peer sync event");
                state.apply_event(&event, Instant::now());
                sync_peers(&state, &network, &local_machine_id).await;
            }
            Some(command) = commands.recv() => {
                debug!(?command, "peer sync command");
                match command {
                    PeerSyncCommand::UpsertTransient(record) => {
                        state.upsert_transient(&record, Instant::now());
                    }
                    PeerSyncCommand::RemoveTransient(id) => state.remove_transient(&id),
                }
                sync_peers(&state, &network, &local_machine_id).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            control_target: None,
            bridge_ip: None,
            endpoints: endpoints.into_iter().map(String::from).collect(),
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn initial_sync_keeps_bootstrap_peer_until_store_catches_up() {
        let network = Arc::new(MemoryWireGuard::new());
        let driver = WireguardDriver::memory_with(network.clone());
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

        tokio::task::yield_now().await;
        cancel.cancel();
        handle.await.expect("peer sync task exits");

        let peers = network.current_peers();
        let [peer] = peers.as_slice() else {
            panic!("expected one peer");
        };
        assert_eq!(peer.id.0, "founder");
    }
}
