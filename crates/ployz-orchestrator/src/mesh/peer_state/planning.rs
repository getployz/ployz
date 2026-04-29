use std::collections::HashMap;

use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::mesh::MeshNetwork;
use crate::model::{MachineId, MachineIdentity, WireGuardPeerSpec};
use std::sync::Arc;

use super::map::PeerStateMap;
use super::peer::PeerState;

pub(crate) async fn sync_peers<N: MeshNetwork>(
    state: &PeerStateMap,
    network: &N,
    local_machine_id: &MachineId,
    endpoint_selections: &Arc<RwLock<HashMap<MachineId, String>>>,
) {
    let selections = endpoint_selections.read().await.clone();
    let planned = plan_mesh_peers(state, local_machine_id, &selections);
    debug!(
        local_machine_id = %local_machine_id,
        peers = ?planned
            .iter()
            .map(|peer| (peer.id(), &peer.endpoints, peer.subnet))
            .collect::<Vec<_>>(),
        "peer sync applying planned wireguard peers"
    );
    if let Err(e) = network.set_peers(&planned).await {
        warn!(?e, "set_peers failed");
    }
}

fn peer_state_to_wireguard_peer_spec(
    ps: &PeerState,
    selected_endpoint: Option<&str>,
) -> WireGuardPeerSpec {
    WireGuardPeerSpec {
        identity: MachineIdentity {
            id: ps.id.clone(),
            public_key: ps.public_key.clone(),
            overlay_ip: ps.overlay_ip,
        },
        subnet: ps.subnet,
        bridge_ip: ps.bridge_ip,
        endpoints: ps.planned_endpoints(selected_endpoint),
    }
}

pub(super) fn plan_mesh_peers(
    state: &PeerStateMap,
    local_machine_id: &MachineId,
    endpoint_selections: &HashMap<MachineId, String>,
) -> Vec<WireGuardPeerSpec> {
    state
        .effective_peers(local_machine_id)
        .filter(|ps| !ps.endpoints.is_empty())
        .map(|peer| {
            peer_state_to_wireguard_peer_spec(
                peer,
                endpoint_selections.get(&peer.id).map(String::as_str),
            )
        })
        .collect()
}
