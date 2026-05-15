use ployz_error::DeployError;
use ployz_error::Error as PloyzError;
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
use ployz_spec::Namespace;

use crate::daemon::node_rpc::{NatsDeployRpcTransport, NatsVolumeZfsRpcTransport};

use super::super::volume_transfer::run_volume_move_rpc;

#[derive(Clone)]
pub(in crate::daemon::handlers::deploy) struct NatsDeployParticipantClient {
    deploy_client: DeployNodeClient<NatsDeployRpcTransport>,
    volume_transport: NatsVolumeZfsRpcTransport,
}

impl NatsDeployParticipantClient {
    #[must_use]
    pub(in crate::daemon::handlers::deploy) fn new(client: NatsNodeRpcClient) -> Self {
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
