use crate::daemon::DaemonState;
use ployz_api::{
    DaemonPayload, DaemonResponse, DeployCandidateStartedPayload, DeployNamespaceSnapshotPayload,
};
use ployz_error::DeployError;
use ployz_error::Error as PloyzError;
use ployz_model::SlotId;
use ployz_model::{DeployId, InstanceId, InstanceStatusRecord, MachineId, MachineMembership};
use ployz_nats::NatsNodeRpcClient;
use ployz_node_runtime::{
    DEPLOY_PARTICIPANT_RPC_POLICY, DEPLOY_VOLUME_CLONE_CLEANUP_RPC_POLICY,
    DEPLOY_VOLUME_CLONE_RPC_POLICY, DEPLOY_VOLUME_MOVE_POLL_INTERVAL,
    DEPLOY_VOLUME_MOVE_START_RPC_POLICY, DEPLOY_VOLUME_MOVE_WAIT_TIMEOUT, DeployNodeClient,
    NodeRpcError, NodeRpcErrorKind,
};
use ployz_orchestrator::deploy::participant::{
    CleanupVolumeCloneRequest, CloneVolumeRequest, CloneVolumeResult, DeployParticipantClient,
    MoveVolumeRequest, MoveVolumeResult, StartCandidateRequest,
};
use ployz_runtime_docker::deploy::remote::DeployAgent;
use ployz_spec::Namespace;

use super::volume_transfer::run_volume_move_rpc;
use crate::daemon::node_rpc::{NatsDeployRpcTransport, NatsVolumeZfsRpcTransport};

impl DaemonState {
    pub async fn handle_deploy_node_inspect_namespace(
        &self,
        namespace: &str,
        _deploy_id: &str,
    ) -> DaemonResponse {
        let namespace = Namespace::new(namespace.to_string());
        let agent = match self.deploy_node_agent().await {
            Ok(agent) => agent,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        match agent.inspect_namespace(&namespace).await {
            Ok(instances) => self.ok_with_payload(
                "namespace inspected",
                Some(DaemonPayload::DeployNamespaceSnapshot(
                    DeployNamespaceSnapshotPayload { instances },
                )),
            ),
            Err(error) => self.err("DEPLOY_NODE_FAILED", error.to_string()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn handle_deploy_node_start_candidate(
        &self,
        namespace: &str,
        deploy_id: &str,
        service: &str,
        slot_id: &str,
        instance_id: &str,
        spec_json: &str,
        volumes_json: &str,
    ) -> DaemonResponse {
        let namespace = match Namespace::try_new(namespace) {
            Ok(namespace) => namespace,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        let deploy_id = match DeployId::try_new(deploy_id) {
            Ok(deploy_id) => deploy_id,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        let slot_id = match SlotId::try_new(slot_id) {
            Ok(slot_id) => slot_id,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        let instance_id = match InstanceId::try_new(instance_id) {
            Ok(instance_id) => instance_id,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        let agent = match self.deploy_node_agent().await {
            Ok(agent) => agent,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        let context = agent.command_context(namespace);
        match agent
            .start_candidate(
                &context,
                service,
                &slot_id,
                &instance_id,
                &deploy_id,
                spec_json,
                volumes_json,
            )
            .await
        {
            Ok(status) => self.ok_with_payload(
                "candidate started",
                Some(DaemonPayload::DeployCandidateStarted(
                    DeployCandidateStartedPayload { status },
                )),
            ),
            Err(error) => self.err("DEPLOY_NODE_FAILED", error.to_string()),
        }
    }

    pub async fn handle_deploy_node_drain_instance(
        &self,
        namespace: &str,
        deploy_id: &str,
        instance_id: &str,
    ) -> DaemonResponse {
        self.handle_deploy_node_instance_command(
            namespace,
            deploy_id,
            instance_id,
            DeployNodeOp::Drain,
        )
        .await
    }

    pub async fn handle_deploy_node_remove_instance(
        &self,
        namespace: &str,
        deploy_id: &str,
        instance_id: &str,
    ) -> DaemonResponse {
        self.handle_deploy_node_instance_command(
            namespace,
            deploy_id,
            instance_id,
            DeployNodeOp::Remove,
        )
        .await
    }

    async fn handle_deploy_node_instance_command(
        &self,
        namespace: &str,
        _deploy_id: &str,
        instance_id: &str,
        op: DeployNodeOp,
    ) -> DaemonResponse {
        let namespace = Namespace::new(namespace.to_string());
        let agent = match self.deploy_node_agent().await {
            Ok(agent) => agent,
            Err(error) => return self.err("DEPLOY_NODE_FAILED", error),
        };
        let context = agent.command_context(namespace);
        let instance_id = InstanceId::new(instance_id.to_string());
        let result = match op {
            DeployNodeOp::Drain => agent.drain_instance(&context, &instance_id).await,
            DeployNodeOp::Remove => agent.remove_instance(&context, &instance_id).await,
        };
        match result {
            Ok(()) => self.ok("deploy node command completed"),
            Err(error) => self.err("DEPLOY_NODE_FAILED", error.to_string()),
        }
    }

    async fn deploy_node_agent(&self) -> Result<DeployAgent, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no mesh is running".to_string())?;
        let store = active.mesh.store.clone();
        let machine_id = self.identity.machine_id.clone();
        let overlay_network_name = self.overlay_network_name();
        let overlay_dns_server = self.overlay_dns_server();
        let storage_driver = self.volume_storage_backend().await?;
        Ok(DeployAgent::new(
            store,
            machine_id,
            overlay_network_name,
            overlay_dns_server,
            storage_driver,
        ))
    }
}

enum DeployNodeOp {
    Drain,
    Remove,
}

#[derive(Clone)]
pub(super) struct NatsDeployParticipantClient {
    deploy_client: DeployNodeClient<NatsDeployRpcTransport>,
    volume_transport: NatsVolumeZfsRpcTransport,
}

impl NatsDeployParticipantClient {
    #[must_use]
    pub(super) fn new(client: NatsNodeRpcClient) -> Self {
        let deploy_client = DeployNodeClient::new(NatsDeployRpcTransport::new(client.clone()))
            .with_policy(DEPLOY_PARTICIPANT_RPC_POLICY);
        Self {
            deploy_client,
            volume_transport: NatsVolumeZfsRpcTransport::new(client),
        }
    }
}

#[async_trait::async_trait]
impl DeployParticipantClient for NatsDeployParticipantClient {
    fn supports_volume_moves(&self) -> bool {
        true
    }

    fn supports_volume_clones(&self) -> bool {
        true
    }

    async fn inspect_namespace(
        &self,
        machine: &MachineMembership,
        namespace: &Namespace,
        deploy_id: &DeployId,
        _coordinator_id: &MachineId,
    ) -> ployz_error::Result<Vec<InstanceStatusRecord>> {
        let payload = self
            .deploy_client
            .inspect_namespace(&machine.id, namespace.as_str(), deploy_id)
            .await
            .map_err(|error| deploy_rpc_error(error, "namespace snapshot"))?;
        Ok(payload.instances)
    }

    async fn start_candidate(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        request: StartCandidateRequest,
    ) -> ployz_error::Result<InstanceStatusRecord> {
        let payload = self
            .deploy_client
            .start_candidate(
                machine_id,
                namespace.as_str(),
                deploy_id,
                &request.service,
                &request.slot_id,
                &request.instance_id,
                &request.spec_json,
                &request.volumes_json,
            )
            .await
            .map_err(|error| deploy_rpc_error(error, "candidate"))?;
        Ok(payload.status)
    }

    async fn move_volume(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        request: MoveVolumeRequest,
    ) -> ployz_error::Result<MoveVolumeResult> {
        run_volume_move_rpc(
            &self.volume_transport,
            machine_id,
            namespace,
            deploy_id,
            request,
            DEPLOY_VOLUME_MOVE_START_RPC_POLICY.timeout,
            DEPLOY_VOLUME_MOVE_WAIT_TIMEOUT,
            DEPLOY_VOLUME_MOVE_POLL_INTERVAL,
        )
        .await
    }

    async fn clone_volume(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        request: CloneVolumeRequest,
    ) -> ployz_error::Result<CloneVolumeResult> {
        let payload = self
            .deploy_client
            .with_policy(DEPLOY_VOLUME_CLONE_RPC_POLICY)
            .clone_volume(
                machine_id,
                namespace.as_str(),
                deploy_id,
                &request.volume,
                request.source_namespace.as_str(),
                &request.source_volume,
                &request.snapshot,
                &request.quota,
                &request.mode,
                &request.owner,
            )
            .await
            .map_err(|error| deploy_rpc_error(error, "volume zfs clone"))?;
        Ok(CloneVolumeResult {
            snapshot: payload.snapshot,
            snapshot_guid: payload.guid,
            target_dataset: payload.target_dataset,
        })
    }

    async fn cleanup_volume_clone(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        request: CleanupVolumeCloneRequest,
    ) -> ployz_error::Result<()> {
        self.deploy_client
            .with_policy(DEPLOY_VOLUME_CLONE_CLEANUP_RPC_POLICY)
            .cleanup_volume_clone(
                machine_id,
                namespace.as_str(),
                deploy_id,
                &request.volume,
                request.source_namespace.as_str(),
                &request.source_volume,
                &request.snapshot,
            )
            .await
            .map_err(|error| deploy_rpc_error(error, "cleanup volume clone"))
    }

    async fn drain_instance(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> ployz_error::Result<()> {
        self.deploy_client
            .drain_instance(machine_id, namespace.as_str(), deploy_id, instance_id)
            .await
            .map_err(|error| deploy_rpc_error(error, "drain instance"))
    }

    async fn remove_instance(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> ployz_error::Result<()> {
        self.deploy_client
            .remove_instance(machine_id, namespace.as_str(), deploy_id, instance_id)
            .await
            .map_err(|error| deploy_rpc_error(error, "remove instance"))
    }
}

fn deploy_rpc_error(error: NodeRpcError, payload: &'static str) -> PloyzError {
    match error.kind {
        NodeRpcErrorKind::MissingPayload | NodeRpcErrorKind::Decode => {
            PloyzError::Deploy(DeployError::MissingNodePayload { payload })
        }
        NodeRpcErrorKind::Transport | NodeRpcErrorKind::Remote => {
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: error.operation,
                code: error.code,
                message: error.message,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deploy_rpc_error_maps_payload_shape_errors_to_missing_payload() {
        assert_eq!(
            deploy_rpc_error(
                NodeRpcError::missing_payload("deploy_node_inspect", "namespace snapshot"),
                "namespace snapshot",
            ),
            PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "namespace snapshot",
            })
        );
        assert_eq!(
            deploy_rpc_error(
                NodeRpcError::decode(
                    "deploy_node_start_candidate",
                    "candidate",
                    "missing field status",
                ),
                "candidate",
            ),
            PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "candidate",
            })
        );
    }

    #[test]
    fn deploy_rpc_error_maps_transport_and_remote_errors_to_remote_node_error() {
        assert_eq!(
            deploy_rpc_error(
                NodeRpcError::transport("deploy_node_drain", "NATS_RPC_TIMEOUT", "timed out"),
                "unused",
            ),
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "deploy_node_drain",
                code: "NATS_RPC_TIMEOUT".into(),
                message: "timed out".into(),
            })
        );
        assert_eq!(
            deploy_rpc_error(
                NodeRpcError::remote(
                    "deploy_node_cleanup_uncommitted_volume_clone",
                    "REMOTE_FAILED",
                    "remote failed",
                ),
                "unused",
            ),
            PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "deploy_node_cleanup_uncommitted_volume_clone",
                code: "REMOTE_FAILED".into(),
                message: "remote failed".into(),
            })
        );
    }
}
