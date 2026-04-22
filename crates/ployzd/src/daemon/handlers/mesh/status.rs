use crate::mesh_state::network::NetworkConfig;
use ployz_api::{DaemonPayload, DaemonResponse};

use super::{DaemonState, mesh_ready_payload};

impl DaemonState {
    pub(crate) fn handle_mesh_list(&self) -> DaemonResponse {
        let networks_dir = self.data_dir.join("networks");
        let entries = match std::fs::read_dir(&networks_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return self.ok("no networks found");
            }
            Err(err) => {
                return self.err("IO_ERROR", format!("failed to read networks dir: {err}"));
            }
        };

        let mut names: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .filter_map(|entry| entry.file_name().to_str().map(ToOwned::to_owned))
            .collect();
        names.sort();

        if names.is_empty() {
            return self.ok("no networks found");
        }

        let running = self.active.as_ref().map(|a| a.config.name.0.as_str());
        let lines: Vec<String> = names
            .iter()
            .map(|name| {
                let state = if running == Some(name.as_str()) {
                    "running"
                } else {
                    "created"
                };
                format!("{name}: {state}")
            })
            .collect();

        self.ok(lines.join("\n"))
    }

    pub(crate) fn handle_mesh_status(&self, network: &str) -> DaemonResponse {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        if !config_path.exists() {
            return self.err(
                "NETWORK_NOT_FOUND",
                format!("network '{network}' does not exist"),
            );
        }

        let config = match NetworkConfig::load(&config_path) {
            Ok(config) => config,
            Err(err) => {
                return self.err("IO_ERROR", format!("failed to load network config: {err}"));
            }
        };

        let running = self
            .active
            .as_ref()
            .is_some_and(|a| a.config.name.0 == network);
        let state = if running { "running" } else { "created" };
        self.ok(format!(
            "network: {}\noverlay: {}\nstate:   {}",
            config.name, config.overlay_ip, state
        ))
    }

    pub(crate) async fn handle_mesh_ready(&self, json: bool) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let Some(self_record) = active.mesh.authoritative_self_record().await else {
            return self.err("SELF_RECORD_MISSING", "mesh self record unavailable");
        };
        let status = mesh_ready_payload(active.mesh.ready_status().await, &self_record);
        if json {
            return match serde_json::to_string(&status) {
                Ok(body) => self.ok_with_payload(body, Some(DaemonPayload::MeshReady(status))),
                Err(err) => self.err(
                    "ENCODE_FAILED",
                    format!("failed to encode readiness payload: {err}"),
                ),
            };
        }

        self.ok_with_payload(format!(
            "ready:                   {}\nphase:                   {}\nstore healthy:           {}\nsync connected:          {}\nparticipation:           {}\nworkload subnet present: {}",
            status.ready,
            status.phase,
            status.store_healthy,
            status.sync_connected,
            status.participation,
            status.workload_subnet_present,
        ), Some(DaemonPayload::MeshReady(status)))
    }
}
