use ployz_api::{DaemonPayload, DaemonResponse, StatusPayload};
use ployz_model::AuthorityNodePosture;

use super::super::DaemonState;

mod control_plane;
mod edge_sync;
mod nats;

#[cfg(test)]
mod tests;

impl DaemonState {
    pub(crate) async fn handle_status(&self) -> DaemonResponse {
        let id = &self.identity;
        match &self.active {
            Some(active) => {
                let local_machine = active.mesh.authoritative_self_record().await;
                let local_machine_lifecycle =
                    local_machine.as_ref().map(|machine| machine.lifecycle);
                let local_authority = local_machine
                    .as_ref()
                    .map(AuthorityNodePosture::from_machine_membership);
                let net = &active.config;
                let nats_assets = self.nats_asset_status().await;
                let mut control_plane = self.control_plane_status().await;
                if let Some(status) =
                    control_plane::storage_replica_intent_status(net.storage_replicas, &nats_assets)
                {
                    control_plane.push(status);
                }
                let payload = StatusPayload {
                    machine_id: id.machine_id.as_str().to_string(),
                    public_key: id.public_key.clone(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    network: Some(net.name.0.clone()),
                    network_lifecycle: Some(net.lifecycle),
                    local_machine_lifecycle,
                    local_authority,
                    overlay_ip: Some(net.overlay_ip.0.to_string()),
                    mesh_phase: format!("{:?}", active.mesh.phase()),
                    edge_sync: self.edge_sync_status().await,
                    nats_assets,
                    control_plane,
                };
                self.ok_with_payload(
                    format!(
                        "machine:            {}\nversion:            {}\nnetwork:            {}\nnetwork lifecycle:  {}\nlocal lifecycle:    {}\noverlay:            {}\nmesh phase:         {:?}",
                        id.machine_id,
                        env!("CARGO_PKG_VERSION"),
                        net.name,
                        net.lifecycle,
                        local_machine_lifecycle
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "unknown".into()),
                        net.overlay_ip,
                        active.mesh.phase(),
                    ),
                    Some(DaemonPayload::Status(payload)),
                )
            }
            None => self.ok_with_payload(
                format!(
                    "machine:            {}\nversion:            {}\nnetwork:            none\nnetwork lifecycle:  —\nlocal lifecycle:    —\nmesh phase:         idle",
                    id.machine_id,
                    env!("CARGO_PKG_VERSION")
                ),
                Some(DaemonPayload::Status(StatusPayload {
                    machine_id: id.machine_id.as_str().to_string(),
                    public_key: id.public_key.clone(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    network: None,
                    network_lifecycle: None,
                    local_machine_lifecycle: None,
                    local_authority: None,
                    overlay_ip: None,
                    mesh_phase: String::from("idle"),
                    edge_sync: Vec::new(),
                    nats_assets: Vec::new(),
                    control_plane: Vec::new(),
                })),
            ),
        }
    }
}
