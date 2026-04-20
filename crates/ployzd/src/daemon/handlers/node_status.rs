use ployz_api::{DaemonPayload, DaemonResponse, NodeStatusPayload, WorkloadSummary};
use ployz_orchestrator::mesh::orchestrator::MeshNodeStatus;
use ployz_types::model::Phase;
use tokio::time::{Duration, Instant};

use super::super::DaemonState;

const WAIT_NODE_PHASE_POLL_INTERVAL: Duration = Duration::from_millis(50);

impl DaemonState {
    pub(crate) async fn handle_node_status(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let payload = to_payload(active.mesh.node_status().await);
        self.ok_with_payload(
            render_node_status(&payload),
            Some(DaemonPayload::NodeStatus(payload)),
        )
    }

    pub(crate) async fn handle_wait_node_phase(
        &self,
        phase: Phase,
        timeout_ms: u64,
    ) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };
        let timeout = Duration::from_millis(timeout_ms);
        let deadline = Instant::now() + timeout;

        loop {
            let payload = to_payload(active.mesh.node_status().await);
            if payload.phase == phase {
                return self.ok_with_payload(
                    render_node_status(&payload),
                    Some(DaemonPayload::NodeStatus(payload)),
                );
            }
            if Instant::now() >= deadline {
                return self.err_with_payload(
                    "PHASE_TIMEOUT",
                    format!(
                        "node did not reach phase '{}' within {}ms (current phase: '{}')",
                        phase, timeout_ms, payload.phase
                    ),
                    Some(DaemonPayload::NodeStatus(payload)),
                );
            }
            tokio::time::sleep(WAIT_NODE_PHASE_POLL_INTERVAL).await;
        }
    }
}

fn to_payload(status: MeshNodeStatus) -> NodeStatusPayload {
    NodeStatusPayload {
        machine_id: status.machine_id.0,
        boot_id: status.boot_id,
        phase: status.phase,
        ready: status.ready,
        drain_state: status.drain_state,
        subnet_claim: status.subnet_claim.map(|subnet| subnet.to_string()),
        workloads: WorkloadSummary {
            slots: status.slot_count,
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}

fn render_node_status(payload: &NodeStatusPayload) -> String {
    format!(
        "machine id:   {}\nboot id:      {}\nphase:        {}\nready:        {}\ndrain state:  {}",
        payload.machine_id, payload.boot_id, payload.phase, payload.ready, payload.drain_state
    )
}
