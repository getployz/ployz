use ployz_api::{DaemonPayload, DaemonResponse, NodeStatusPayload, WorkloadSummary};
use ployz_orchestrator::mesh::orchestrator::MeshNodeStatus;

use super::super::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_node_status(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let payload = to_payload(active.mesh.node_status().await);
        self.ok_with_payload(
            format!(
                "machine id:  {}\nboot id:     {}\nphase:       {}\nready:       {}\ndraining:    {}",
                payload.machine_id, payload.boot_id, payload.phase, payload.ready, payload.draining
            ),
            Some(DaemonPayload::NodeStatus(payload)),
        )
    }
}

fn to_payload(status: MeshNodeStatus) -> NodeStatusPayload {
    NodeStatusPayload {
        machine_id: status.machine_id.0,
        boot_id: status.boot_id,
        phase: status.phase.to_string(),
        ready: status.ready,
        draining: status.draining,
        subnet_claim: status.subnet_claim.map(|subnet| subnet.to_string()),
        workloads: WorkloadSummary {
            slots: status.slot_count,
        },
        version: env!("CARGO_PKG_VERSION").to_string(),
    }
}
