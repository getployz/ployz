use crate::daemon::DaemonState;
use crate::endpoint_maintenance::local_endpoint_watch_supported;
use ployz_api::DaemonPayload;
use ployz_host_backends::network::endpoints::detect_advertised_endpoints;
use ployz_orchestrator::mesh::WireGuardDevice;
use ployz_orchestrator::mesh::wireguard::DEFAULT_LISTEN_PORT;
use ployz_store_api::{MachineMembershipStore, PeerRttStore};

mod render;
mod report;

#[cfg(test)]
mod tests;

use render::render_doctor_report;
use report::build_doctor_payload;

impl DaemonState {
    pub(crate) async fn handle_doctor(&self) -> ployz_api::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_RUNNING_NETWORK", "no mesh running");
        };

        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(err) => return self.err("LIST_FAILED", format!("failed to list machines: {err}")),
        };

        let local_record = match machines
            .iter()
            .find(|machine| machine.id == self.identity.machine_id)
        {
            Some(record) => record,
            None => {
                return self.err(
                    "SELF_RECORD_MISSING",
                    format!(
                        "local machine '{}' is not published in the store",
                        self.identity.machine_id
                    ),
                );
            }
        };

        let device_peers = match active.mesh.network.read_peers().await {
            Ok(peers) => peers,
            Err(err) => {
                return self.err(
                    "WIREGUARD_READ_FAILED",
                    format!("failed to read local wireguard peers: {err}"),
                );
            }
        };
        let rtt_observations = match active.mesh.store.peer_rtt_observations().await {
            Ok(observations) => observations,
            Err(err) => {
                return self.err(
                    "PEER_RTT_READ_FAILED",
                    format!("failed to read peer RTT observations: {err}"),
                );
            }
        };
        let detected_local_endpoints = detect_advertised_endpoints(DEFAULT_LISTEN_PORT).await;
        let payload = build_doctor_payload(
            active,
            &machines,
            local_record,
            &device_peers,
            &rtt_observations,
            &detected_local_endpoints,
            local_endpoint_watch_supported(),
        );

        self.ok_with_payload(
            render_doctor_report(&payload),
            Some(DaemonPayload::Doctor(payload)),
        )
    }
}
