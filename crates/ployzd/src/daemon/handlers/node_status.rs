use super::super::DaemonState;
use ployz_api::{DaemonPayload, DaemonResponse, NodeStatusPayload};
use ployz_types::model::Participation;

impl DaemonState {
    pub(crate) async fn handle_node_status(&self) -> DaemonResponse {
        let Some(active) = &self.active else {
            return self.err("MESH_NOT_ACTIVE", "mesh is not active");
        };

        let ready = active.mesh.ready_status().await;
        let self_record = active
            .store
            .machine()
            .list_machines()
            .await
            .ok()
            .and_then(|machines| {
                machines
                    .into_iter()
                    .find(|machine| machine.id == self.identity.machine_id)
            });

        let (draining, subnet_claim) = match self_record {
            Some(record) => (
                record.participation == Participation::Draining,
                record.subnet.map(|subnet| subnet.to_string()),
            ),
            None => (false, None),
        };

        let payload = NodeStatusPayload {
            machine_id: self.identity.machine_id.clone(),
            boot_id: self.boot_id.clone(),
            phase: format!("{:?}", ready.phase).to_lowercase(),
            ready: ready.ready,
            draining,
            subnet_claim,
            version: env!("CARGO_PKG_VERSION").to_string(),
        };

        self.ok_with_payload(
            format!(
                "machine={} phase={} ready={} draining={}",
                payload.machine_id, payload.phase, payload.ready, payload.draining
            ),
            Some(DaemonPayload::NodeStatus(payload)),
        )
    }
}
