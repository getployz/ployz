use super::teardown;
use crate::daemon::node_rpc::NatsMeshRpcTransport;
use crate::daemon::{ActiveMesh, DaemonState};
use crate::mesh_state::network::NetworkConfig;
use ployz_model::{MachineId, MachineMembership, NetworkId};
use ployz_node_runtime::{
    MESH_DESTRUCTIVE_RPC_POLICY, MeshNodeClient, NodeRpcError, NodeRpcErrorKind,
};
use ployz_store_api::MachineMembershipStore;
use tokio::sync::oneshot;
use tracing::{error, warn};

impl DaemonState {
    pub(crate) async fn handle_mesh_destroy(&mut self, network: &str) -> ployz_api::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            let config_path = NetworkConfig::path(&self.data_dir, network);
            if !config_path.exists() {
                return self.err(
                    "NETWORK_NOT_FOUND",
                    format!("network '{network}' does not exist"),
                );
            }
            if let Err(e) = NetworkConfig::delete(&self.data_dir, network) {
                return self.err("IO_ERROR", format!("failed to delete network config: {e}"));
            }
            return self.ok(format!("mesh '{network}' destroyed"));
        };

        if active.config.name.0 != network {
            return self.err(
                "NETWORK_NOT_RUNNING",
                format!(
                    "network '{network}' is not running; active network is '{}'",
                    active.config.name
                ),
            );
        }

        let rpc_client = match self.nats_node_rpc_client().await {
            Ok(client) => client,
            Err(error) => return self.err("NATS_RPC_UNAVAILABLE", error),
        };
        let network_id = active.config.id.clone();
        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => {
                return self.err(
                    "LIST_FAILED",
                    format!("failed to list machines before destroy: {error}"),
                );
            }
        };
        let operation_id = format!("mesh-destroy-{}", NetworkId::random());
        let expected_machine_ids = sorted_machine_ids(&machines);
        let peers = machines
            .iter()
            .filter(|machine| machine.id != self.identity.machine_id)
            .cloned()
            .collect::<Vec<_>>();
        let mesh_client = MeshNodeClient::new(NatsMeshRpcTransport::new(rpc_client));

        let mut prepared = Vec::new();
        let mut failures = Vec::new();
        for peer in &peers {
            let response = mesh_client
                .prepare_destroy(
                    &peer.id,
                    &operation_id,
                    &network_id,
                    &self.identity.machine_id,
                    &expected_machine_ids,
                )
                .await;
            match response {
                Ok(()) => prepared.push(peer.clone()),
                Err(error) => failures.push(mesh_prepare_failure_message(&peer.id, &error)),
            }
        }

        if !failures.is_empty() {
            for peer in &prepared {
                if let Err(error) = mesh_client.cancel_destroy(&peer.id, &operation_id).await {
                    warn!(peer = %peer.id, %operation_id, error = %error, "mesh destroy cancel failed");
                }
            }
            return self.err(
                "MESH_DESTROY_PEERS_UNREACHABLE",
                format!(
                    "mesh destroy refused; all registered peers must confirm teardown first: {}",
                    failures.join("; ")
                ),
            );
        }

        let execute_client = mesh_client.with_policy(MESH_DESTRUCTIVE_RPC_POLICY);
        let mut execute_failures = Vec::new();
        for peer in &prepared {
            match execute_client
                .execute_destroy(&peer.id, &operation_id, &network_id)
                .await
            {
                Ok(()) => {}
                Err(error) => {
                    execute_failures.push(format!("{} execute failed: {error}", peer.id));
                    warn!(peer = %peer.id, %operation_id, error = %error, "mesh destroy execute failed");
                }
            }
        }
        if !execute_failures.is_empty() {
            return self.err(
                "MESH_DESTROY_PEER_EXECUTE_FAILED",
                format!(
                    "peer execute failed after prepare succeeded: {}",
                    execute_failures.join("; ")
                ),
            );
        }

        match self.destroy_local_mesh_runtime(&network_id).await {
            Ok(()) => self.ok(format!("mesh '{network}' destroyed")),
            Err(error) => self.err("NETWORK_DESTROY_FAILED", error),
        }
    }

    pub(crate) async fn handle_mesh_peer_prepare_destroy(
        &self,
        operation_id: &str,
        network_id: &NetworkId,
        coordinator_id: &MachineId,
        expected_machine_ids: &[MachineId],
    ) -> ployz_api::DaemonResponse {
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => return *response,
        };
        if active.config.id != *network_id {
            return self.err(
                "NETWORK_MISMATCH",
                format!(
                    "destroy operation '{operation_id}' targets network '{}', local network is '{}'",
                    network_id, active.config.id
                ),
            );
        }

        let machines = match active.mesh.store.list_machines().await {
            Ok(machines) => machines,
            Err(error) => {
                return self.err(
                    "LIST_FAILED",
                    format!("failed to list machines before prepare: {error}"),
                );
            }
        };
        let local_ids = sorted_machine_ids(&machines);
        let mut expected = expected_machine_ids.to_vec();
        expected.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        if local_ids != expected {
            return self.err(
                "MACHINE_SET_MISMATCH",
                format!(
                    "destroy operation '{operation_id}' from '{coordinator_id}' has a different machine set"
                ),
            );
        }

        self.ok(format!(
            "mesh destroy operation '{operation_id}' prepared on '{}'",
            self.identity.machine_id
        ))
    }

    pub(crate) async fn handle_mesh_peer_cancel_destroy(
        &self,
        operation_id: &str,
    ) -> ployz_api::DaemonResponse {
        self.ok(format!(
            "mesh destroy operation '{operation_id}' cancelled on '{}'",
            self.identity.machine_id
        ))
    }

    pub(crate) async fn handle_mesh_peer_execute_destroy(
        &mut self,
        operation_id: &str,
        network_id: &NetworkId,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> ployz_api::DaemonResponse {
        match self.take_active_for_destroy(network_id) {
            Ok(active) => {
                self.revoke_image_sessions_for_teardown("mesh destroy")
                    .await;
                let data_dir = self.data_dir.clone();
                let machine_id = self.identity.machine_id.clone();
                let operation_id_for_log = operation_id.to_string();
                let network_id_for_log = network_id.clone();
                teardown::spawn_teardown_after_response(
                    response_flushed,
                    async move { teardown::perform_mesh_teardown(data_dir, active).await },
                    move |error| {
                        error!(
                            operation_id = %operation_id_for_log,
                            network_id = %network_id_for_log,
                            %machine_id,
                            %error,
                            "mesh destroy teardown failed after peer ack"
                        );
                    },
                );
                self.ok(format!(
                    "mesh destroy operation '{operation_id}' executed on '{}'",
                    self.identity.machine_id
                ))
            }
            Err(error) => self.err("NETWORK_DESTROY_FAILED", error),
        }
    }

    pub(crate) async fn handle_mesh_peer_remove_machine(
        &mut self,
        operation_id: &str,
        network_id: &NetworkId,
        machine_id: &MachineId,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> ployz_api::DaemonResponse {
        if *machine_id != self.identity.machine_id {
            return self.err(
                "MACHINE_MISMATCH",
                format!(
                    "remove operation '{operation_id}' targets machine '{machine_id}', local machine is '{}'",
                    self.identity.machine_id
                ),
            );
        }

        match self.take_active_for_destroy(network_id) {
            Ok(active) => {
                self.revoke_image_sessions_for_teardown("machine remove")
                    .await;
                let data_dir = self.data_dir.clone();
                let local_machine_id = self.identity.machine_id.clone();
                let operation_id_for_log = operation_id.to_string();
                let network_id_for_log = network_id.clone();
                teardown::spawn_teardown_after_response(
                    response_flushed,
                    async move { teardown::perform_mesh_teardown(data_dir, active).await },
                    move |error| {
                        error!(
                            operation_id = %operation_id_for_log,
                            network_id = %network_id_for_log,
                            machine_id = %local_machine_id,
                            %error,
                            "machine remove teardown failed after peer ack"
                        );
                    },
                );
                self.ok(format!(
                    "machine remove operation '{operation_id}' executed on '{}'",
                    self.identity.machine_id
                ))
            }
            Err(error) => self.err("MACHINE_REMOVE_FAILED", error),
        }
    }

    async fn destroy_local_mesh_runtime(&mut self, network_id: &NetworkId) -> Result<(), String> {
        let active = self.take_active_for_destroy(network_id)?;
        self.revoke_image_sessions_for_teardown("mesh destroy")
            .await;
        teardown::perform_mesh_teardown(self.data_dir.clone(), active).await
    }

    async fn revoke_image_sessions_for_teardown(&self, operation: &'static str) {
        if let Err(error) = self.image_registry.revoke_all_sessions().await {
            warn!(%error, operation, "image receive session cleanup failed during mesh teardown");
        }
    }

    fn take_active_for_destroy(&mut self, network_id: &NetworkId) -> Result<ActiveMesh, String> {
        let active_id = {
            let Some(active) = self.active.as_ref() else {
                return Err("no mesh running".to_string());
            };
            if active.config.id != *network_id {
                return Err(format!(
                    "operation targets network '{}', local network is '{}'",
                    network_id, active.config.id
                ));
            }
            active.config.id.clone()
        };

        let Some(active) = self.active.take() else {
            return Err("no mesh running".to_string());
        };
        if active.config.id != active_id {
            self.active = Some(active);
            return Err("active network changed during destroy".to_string());
        }
        Ok(active)
    }
}

fn sorted_machine_ids(machines: &[MachineMembership]) -> Vec<MachineId> {
    let mut ids = machines
        .iter()
        .map(|machine| machine.id.clone())
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    ids
}

pub(in crate::daemon::handlers::mesh) fn mesh_prepare_failure_message(
    peer_id: &MachineId,
    error: &NodeRpcError,
) -> String {
    if error.kind == NodeRpcErrorKind::Remote {
        return format!(
            "{peer_id} rejected prepare [{}]: {}",
            error.code, error.message
        );
    }
    format!("{peer_id} unreachable: {error}")
}
