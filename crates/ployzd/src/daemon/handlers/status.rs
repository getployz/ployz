use ployz_api::{DaemonPayload, DaemonResponse, StatusPayload};

use super::super::DaemonState;

impl DaemonState {
    pub(crate) fn handle_status(&self) -> DaemonResponse {
        let id = &self.identity;
        match &self.active {
            Some(active) => {
                let net = &active.config;
                let payload = StatusPayload {
                    machine_id: id.machine_id.0.clone(),
                    network: Some(net.name.0.clone()),
                    overlay_ip: Some(net.overlay_ip.0.to_string()),
                    phase: format!("{:?}", active.mesh.phase()),
                };
                self.ok_with_payload(format!(
                    "machine:  {}\nnetwork:  {}\noverlay:  {}\nphase:    {:?}",
                    id.machine_id,
                    net.name,
                    net.overlay_ip,
                    active.mesh.phase(),
                ), Some(DaemonPayload::Status(payload)))
            }
            None => self.ok_with_payload(
                format!("machine:  {}\nnetwork:  none\nphase:    idle", id.machine_id),
                Some(DaemonPayload::Status(StatusPayload {
                    machine_id: id.machine_id.0.clone(),
                    network: None,
                    overlay_ip: None,
                    phase: String::from("idle"),
                })),
            ),
        }
    }
}
