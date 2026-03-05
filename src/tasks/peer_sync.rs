use crate::backends::WireguardDriver;
use crate::mesh::peer_state::{PeerStateMap, sync_peers};
use crate::model::{MachineEvent, MachineRecord};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

pub(crate) async fn run_peer_sync_task(
    snapshot: Vec<MachineRecord>,
    mut events: mpsc::Receiver<MachineEvent>,
    network: WireguardDriver,
    cancel: CancellationToken,
) {
    let mut state = PeerStateMap::new();
    state.init_from_snapshot(&snapshot);
    sync_peers(&state, &network).await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("peer sync task cancelled");
                break;
            }
            Some(event) = events.recv() => {
                debug!(?event, "peer sync event");
                state.apply_event(&event);
                sync_peers(&state, &network).await;
            }
        }
    }
}
