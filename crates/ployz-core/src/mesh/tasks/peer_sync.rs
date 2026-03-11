use crate::mesh::driver::WireguardDriver;
use crate::mesh::peer_state::{PeerStateMap, sync_peers};
use crate::model::{MachineEvent, MachineId, MachineRecord};
use tokio::sync::mpsc;
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
    network: WireguardDriver,
    local_machine_id: MachineId,
    cancel: CancellationToken,
) {
    let mut state = PeerStateMap::new();
    state.init_from_snapshot(&snapshot);
    sync_peers(&state, &network, &local_machine_id).await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("peer sync task cancelled");
                break;
            }
            Some(event) = events.recv() => {
                debug!(?event, "peer sync event");
                state.apply_event(&event);
                sync_peers(&state, &network, &local_machine_id).await;
            }
            Some(command) = commands.recv() => {
                debug!(?command, "peer sync command");
                match command {
                    PeerSyncCommand::UpsertTransient(record) => state.upsert_transient(&record),
                    PeerSyncCommand::RemoveTransient(id) => state.remove_transient(&id),
                }
                sync_peers(&state, &network, &local_machine_id).await;
            }
        }
    }
}
