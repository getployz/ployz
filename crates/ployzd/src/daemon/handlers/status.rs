use ployz_api::{DaemonPayload, DaemonResponse, StatusPayload};

use super::super::DaemonState;

const DAEMON_PROTOCOL_VERSION: u32 = 1;

fn daemon_capabilities() -> Vec<String> {
    vec![
        "status-payload-v1".into(),
        "mesh-status-payload-v1".into(),
        "mesh-ready-payload-v1".into(),
        "mesh-self-record-payload-v1".into(),
        "machine-list-payload-v1".into(),
        "machine-operation-payload-v1".into(),
    ]
}

impl DaemonState {
    pub(crate) fn handle_status(&self) -> DaemonResponse {
        let id = &self.identity;
        match &self.active {
            Some(active) => {
                let net = &active.config;
                let payload = StatusPayload {
                    protocol_version: DAEMON_PROTOCOL_VERSION,
                    daemon_version: env!("CARGO_PKG_VERSION").into(),
                    machine_id: id.machine_id.0.clone(),
                    active_network_name: Some(net.name.0.clone()),
                    phase: format!("{:?}", active.mesh.phase()).to_lowercase(),
                    capabilities: daemon_capabilities(),
                };

                self.ok_with_payload(
                    format!(
                        "machine:  {}\nnetwork:  {}\noverlay:  {}\nphase:    {:?}",
                        id.machine_id,
                        net.name,
                        net.overlay_ip,
                        active.mesh.phase(),
                    ),
                    Some(DaemonPayload::Status(payload)),
                )
            }
            None => self.ok_with_payload(
                format!(
                    "machine:  {}\nnetwork:  none\nphase:    idle",
                    id.machine_id
                ),
                Some(DaemonPayload::Status(StatusPayload {
                    protocol_version: DAEMON_PROTOCOL_VERSION,
                    daemon_version: env!("CARGO_PKG_VERSION").into(),
                    machine_id: id.machine_id.0.clone(),
                    active_network_name: None,
                    phase: "idle".into(),
                    capabilities: daemon_capabilities(),
                })),
            ),
        }
    }
}
