use tracing::{debug, warn};

use crate::mesh::MeshNetwork;
use crate::model::{MachineId, MachineRecord, MachineStatus, Participation};

use super::map::PeerStateMap;
use super::peer::PeerState;

pub(crate) async fn sync_peers<N: MeshNetwork>(
    state: &PeerStateMap,
    network: &N,
    local_machine_id: &MachineId,
) {
    let planned = plan_mesh_peers(state, local_machine_id);
    debug!(
        local_machine_id = %local_machine_id,
        peers = ?planned
            .iter()
            .map(|peer| (&peer.id, &peer.endpoints, peer.subnet))
            .collect::<Vec<_>>(),
        "peer sync applying planned wireguard peers"
    );
    if let Err(e) = network.set_peers(&planned).await {
        warn!(?e, "set_peers failed");
    }
}

fn peer_state_to_planned_record(ps: &PeerState) -> MachineRecord {
    MachineRecord {
        id: ps.id.clone(),
        public_key: ps.public_key.clone(),
        overlay_ip: ps.overlay_ip,
        control_target: None,
        subnet: ps.subnet,
        bridge_ip: ps.bridge_ip,
        endpoints: ps.planned_endpoints(),
        status: MachineStatus::Unknown,
        participation: Participation::Disabled,
        last_heartbeat: 0,
        created_at: 0,
        updated_at: 0,
        labels: std::collections::BTreeMap::new(),
    }
}

pub(super) fn plan_mesh_peers(
    state: &PeerStateMap,
    local_machine_id: &MachineId,
) -> Vec<MachineRecord> {
    let mut planned: Vec<MachineRecord> = state
        .stored_peers
        .values()
        .filter(|ps| ps.id != *local_machine_id)
        .filter(|ps| !ps.runtime.endpoints.is_empty())
        .map(peer_state_to_planned_record)
        .collect();

    planned.extend(
        state
            .transient_peers
            .values()
            .filter(|ps| ps.id != *local_machine_id)
            .filter(|ps| !state.stored_peers.contains_key(&ps.id))
            .filter(|ps| !ps.runtime.endpoints.is_empty())
            .map(peer_state_to_planned_record),
    );

    planned
}
