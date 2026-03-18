use ployz_api::DaemonResponse;

use super::super::DaemonState;

impl DaemonState {
    pub(crate) fn handle_status(&self) -> DaemonResponse {
        let id = &self.identity;
        match &self.active {
            Some(active) => {
                let net = &active.config;
                self.ok(format!(
                    "machine:  {}\nnetwork:  {}\noverlay:  {}\nphase:    {:?}",
                    id.machine_id,
                    net.name,
                    net.overlay_ip,
                    active.mesh.phase(),
                ))
            }
            None => self.ok(format!(
                "machine:  {}\nnetwork:  none\nphase:    idle",
                id.machine_id,
            )),
        }
    }
}
