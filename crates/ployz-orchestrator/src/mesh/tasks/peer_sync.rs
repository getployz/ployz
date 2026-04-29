use crate::mesh::driver::WireguardDriver;
use crate::mesh::peer_state::{PeerStateMap, sync_peers};
use crate::mesh::tasks::EndpointSelectionMap;
use crate::model::{MachineEvent, MachineId, MachineMembership, MachineObservation};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

#[derive(Debug, Clone)]
pub enum PeerSyncCommand {
    UpsertTransient(MachineObservation),
    RemoveTransient(MachineId),
}

pub(crate) struct PeerSyncTask {
    pub(crate) snapshot: Vec<MachineMembership>,
    pub(crate) events: mpsc::Receiver<MachineEvent>,
    pub(crate) commands: mpsc::Receiver<PeerSyncCommand>,
    pub(crate) bootstrap_peers: Vec<MachineObservation>,
    pub(crate) network: WireguardDriver,
    pub(crate) local_machine_id: MachineId,
    pub(crate) endpoint_selections: EndpointSelectionMap,
    pub(crate) cancel: CancellationToken,
}

pub(crate) async fn run_peer_sync_task(task: PeerSyncTask) {
    let PeerSyncTask {
        snapshot,
        mut events,
        mut commands,
        bootstrap_peers,
        network,
        local_machine_id,
        endpoint_selections,
        cancel,
    } = task;
    let mut state = PeerStateMap::new();
    let now = Instant::now();
    state.init_from_snapshot(&snapshot, now);
    for observation in &bootstrap_peers {
        if observation.id() != &local_machine_id {
            state.upsert_transient(observation, now);
        }
    }
    sync_peers(&state, &network, &local_machine_id, &endpoint_selections).await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("peer sync task cancelled");
                break;
            }
            Some(event) = events.recv() => {
                debug!(?event, "peer sync event");
                state.apply_event(&event, Instant::now());
                sync_peers(&state, &network, &local_machine_id, &endpoint_selections).await;
            }
            Some(command) = commands.recv() => {
                debug!(?command, "peer sync command");
                match command {
                    PeerSyncCommand::UpsertTransient(observation) => {
                        state.upsert_transient(&observation, Instant::now());
                    }
                    PeerSyncCommand::RemoveTransient(id) => state.remove_transient(&id),
                }
                sync_peers(&state, &network, &local_machine_id, &endpoint_selections).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::driver::WireguardDriver;
    use crate::mesh::wireguard::MemoryWireGuard;
    use crate::model::{MachineEvent, MachineLifecycle, MachineTopology, OverlayIp, PublicKey};
    use std::collections::HashMap;
    use std::net::Ipv6Addr;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    fn test_record(id: &str, key: PublicKey, endpoints: Vec<&str>) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: key,
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            subnet: None,
            control_target: None,
            bridge_ip: None,
            endpoints: endpoints.into_iter().map(String::from).collect(),
            lifecycle: MachineLifecycle::Standby,
            created_at: 0,
            updated_at: 0,
            labels: std::collections::BTreeMap::new(),
        }
    }

    fn test_observation(id: &str, key: PublicKey, endpoints: Vec<&str>) -> MachineObservation {
        MachineObservation::seed(
            MachineId(id.into()),
            key,
            OverlayIp(Ipv6Addr::LOCALHOST),
            None,
            endpoints.into_iter().map(String::from).collect(),
        )
    }

    #[tokio::test]
    async fn initial_sync_keeps_bootstrap_peer_until_store_catches_up() {
        let network = Arc::new(MemoryWireGuard::new());
        let driver = WireguardDriver::memory_with(network.clone());
        let local_machine_id = MachineId("joiner".into());
        let snapshot = vec![test_record("joiner", PublicKey([1; 32]), vec!["self:1"])];
        let bootstrap_peers = vec![test_observation(
            "founder",
            PublicKey([2; 32]),
            vec!["founder:1"],
        )];
        let (_event_tx, event_rx) = mpsc::channel::<MachineEvent>(4);
        let (_command_tx, command_rx) = mpsc::channel::<PeerSyncCommand>(4);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();

        let handle = tokio::spawn(async move {
            run_peer_sync_task(PeerSyncTask {
                snapshot,
                events: event_rx,
                commands: command_rx,
                bootstrap_peers,
                network: driver,
                local_machine_id,
                endpoint_selections: Arc::new(RwLock::new(HashMap::new())),
                cancel: task_cancel,
            })
            .await;
        });

        tokio::task::yield_now().await;
        cancel.cancel();
        handle.await.expect("peer sync task exits");

        let peers = network.current_peers();
        let [peer] = peers.as_slice() else {
            panic!("expected one peer");
        };
        assert_eq!(peer.id().0, "founder");
    }
}
