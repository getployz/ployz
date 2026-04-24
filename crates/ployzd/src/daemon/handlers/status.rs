use ployz_api::{DaemonPayload, DaemonResponse, StatusPayload};

use super::super::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_status(&self) -> DaemonResponse {
        let id = &self.identity;
        match &self.active {
            Some(active) => {
                let local_machine_lifecycle = active
                    .mesh
                    .authoritative_self_record()
                    .await
                    .map(|machine| machine.lifecycle);
                let net = &active.config;
                let payload = StatusPayload {
                    machine_id: id.machine_id.0.clone(),
                    network: Some(net.name.0.clone()),
                    network_lifecycle: Some(net.lifecycle),
                    local_machine_lifecycle,
                    overlay_ip: Some(net.overlay_ip.0.to_string()),
                    mesh_phase: format!("{:?}", active.mesh.phase()),
                };
                self.ok_with_payload(
                    format!(
                        "machine:            {}\nnetwork:            {}\nnetwork lifecycle:  {}\nlocal lifecycle:    {}\noverlay:            {}\nmesh phase:         {:?}",
                        id.machine_id,
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
                    "machine:            {}\nnetwork:            none\nnetwork lifecycle:  —\nlocal lifecycle:    —\nmesh phase:         idle",
                    id.machine_id
                ),
                Some(DaemonPayload::Status(StatusPayload {
                    machine_id: id.machine_id.0.clone(),
                    network: None,
                    network_lifecycle: None,
                    local_machine_lifecycle: None,
                    overlay_ip: None,
                    mesh_phase: String::from("idle"),
                })),
            ),
        }
    }
}
