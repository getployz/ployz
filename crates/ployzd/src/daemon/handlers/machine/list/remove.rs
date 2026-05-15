use crate::daemon::DaemonState;
use crate::daemon::node_rpc::NatsMeshRpcTransport;
use crate::mesh_state::bootstrap::refresh_bootstrap_peer_records_from_store;
use ployz_api::{DaemonPayload, DaemonResponse, MachineRemovePayload};
use ployz_model::MachineId;
use ployz_node_runtime::{MESH_MACHINE_REMOVE_RPC_POLICY, MeshNodeClient, NodeRpcErrorKind};
use ployz_store_api::MachineMembershipStore;

use super::find_machine_record;

impl DaemonState {
    pub(crate) async fn handle_machine_remove(&self, id: &str, force: bool) -> DaemonResponse {
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => return *response,
        };

        let machine_id = match MachineId::try_new(id) {
            Ok(machine_id) => machine_id,
            Err(error) => return self.err("MACHINE_INVALID_TARGET", error),
        };
        let record = match find_machine_record(&active.mesh.store, &machine_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err("MACHINE_NOT_FOUND", format!("machine '{id}' not found"));
            }
            Err(err) => {
                return self.err("LIST_FAILED", format!("failed to read machines: {err}"));
            }
        };

        if record.id == self.identity.machine_id {
            return self.err(
                "CANNOT_REMOVE_SELF",
                "cannot remove the local machine with `machine rm`; use `mesh destroy` for whole-mesh teardown",
            );
        }

        if !force {
            let rpc_client = match self.nats_node_rpc_client().await {
                Ok(client) => MeshNodeClient::new(NatsMeshRpcTransport::new(client))
                    .with_policy(MESH_MACHINE_REMOVE_RPC_POLICY),
                Err(error) => {
                    return self.err(
                        "MACHINE_REMOVE_PEER_UNREACHABLE",
                        format!(
                            "machine '{id}' did not confirm online removal; rerun with --force for membership-record-only removal: {error}"
                        ),
                    );
                }
            };
            let operation_id = format!("machine-rm-{}", ployz_model::NetworkId::random());
            let response = rpc_client
                .remove_machine(&record.id, &operation_id, &active.config.id, &record.id)
                .await;
            match response {
                Ok(()) => {}
                Err(error) if error.kind == NodeRpcErrorKind::Remote => {
                    return self.err(
                        "MACHINE_REMOVE_PEER_REJECTED",
                        format!(
                            "machine '{id}' rejected coordinated removal [{}]: {}; resolve the remote failure or rerun with --force only if you intend membership-record-only removal",
                            error.code, error.message
                        ),
                    );
                }
                Err(error) => {
                    return self.err(
                        "MACHINE_REMOVE_PEER_UNREACHABLE",
                        format!(
                            "machine '{id}' did not confirm online removal; rerun with --force for membership-record-only removal: {error}"
                        ),
                    );
                }
            }
        }

        match active.mesh.store.delete_machine(&machine_id).await {
            Ok(()) => {
                let network_dir = self.network_dir(&active.config.name.0);
                if let Err(error) = refresh_bootstrap_peer_records_from_store(
                    &network_dir,
                    &active.mesh.store,
                    &self.identity.machine_id,
                )
                .await
                {
                    tracing::warn!(
                        %machine_id,
                        %error,
                        "failed to refresh bootstrap peer seed after machine remove"
                    );
                }
                self.ok_with_payload(
                    format!("machine '{id}' removed"),
                    Some(DaemonPayload::MachineRemove(MachineRemovePayload {
                        id: id.to_string(),
                        force,
                    })),
                )
            }
            Err(err) => self.err("DELETE_FAILED", format!("failed to remove machine: {err}")),
        }
    }
}
