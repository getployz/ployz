use crate::daemon::DaemonState;
use ployz_api::{
    DaemonPayload, DaemonResponse, DeployCandidateStartedPayload, DeployNamespaceSnapshotPayload,
};
use ployz_error::DeployError;
use ployz_error::Error as PloyzError;
use ployz_model::SlotId;
use ployz_model::{DeployId, InstanceId, InstanceStatusRecord, MachineId, MachineMembership};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcPolicy};
use ployz_node_api::NodeRequest;
use ployz_orchestrator::deploy::participant::{
    CleanupVolumeCloneRequest, CloneVolumeRequest, CloneVolumeResult, DeployParticipantClient,
    MoveVolumeRequest, MoveVolumeResult, StartCandidateRequest,
};
use ployz_runtime_docker::deploy::remote::DeployAgent;
use ployz_spec::Namespace;

use super::volume_transfer::run_volume_move_rpc;
use crate::daemon::node_rpc::NatsVolumeZfsRpcTransport;

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
    client: NatsNodeRpcClient,
}

impl NatsDeployParticipantClient {
    #[must_use]
    pub(super) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
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
        let response = self
            .client
            .request(
                NodeCommandSubject::deploy_inspect_namespace(&machine.id),
                &NodeRequest::DeployNodeInspectNamespace {
                    namespace: namespace.as_str().to_string(),
                    deploy_id: deploy_id.as_str().to_string(),
                },
            )
            .await
            .map_err(PloyzError::from)?;
        if !response.is_ok() {
            return Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "deploy_node_inspect",
                code: response.code().to_string(),
                message: response.message().to_string(),
            }));
        }
        let Some(DaemonPayload::DeployNamespaceSnapshot(payload)) = response.payload() else {
            return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "namespace snapshot",
            }));
        };
        Ok(payload.instances)
    }

    async fn start_candidate(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        request: StartCandidateRequest,
    ) -> ployz_error::Result<InstanceStatusRecord> {
        let response = self
            .client
            .request(
                NodeCommandSubject::deploy_start_candidate(machine_id),
                &NodeRequest::DeployNodeStartCandidate {
                    namespace: namespace.as_str().to_string(),
                    deploy_id: deploy_id.as_str().to_string(),
                    service: request.service,
                    slot_id: request.slot_id.as_str().to_string(),
                    instance_id: request.instance_id.as_str().to_string(),
                    spec_json: request.spec_json,
                    volumes_json: request.volumes_json,
                },
            )
            .await
            .map_err(PloyzError::from)?;
        if !response.is_ok() {
            return Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "deploy_node_start_candidate",
                code: response.code().to_string(),
                message: response.message().to_string(),
            }));
        }
        let Some(DaemonPayload::DeployCandidateStarted(payload)) = response.payload() else {
            return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "candidate",
            }));
        };
        Ok(payload.status)
    }

    async fn move_volume(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        request: MoveVolumeRequest,
    ) -> ployz_error::Result<MoveVolumeResult> {
        let volume_transport = NatsVolumeZfsRpcTransport::new(self.client.clone());
        run_volume_move_rpc(
            &volume_transport,
            machine_id,
            namespace,
            deploy_id,
            request,
            super::DEPLOY_VOLUME_MOVE_START_RPC_TIMEOUT,
            super::DEPLOY_VOLUME_MOVE_RPC_TIMEOUT,
            super::DEPLOY_VOLUME_MOVE_POLL_INTERVAL,
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
        let response = self
            .client
            .clone()
            .with_policy(RpcPolicy {
                timeout: super::DEPLOY_VOLUME_CLONE_RPC_TIMEOUT,
            })
            .request(
                NodeCommandSubject::deploy_clone_volume(machine_id),
                &NodeRequest::DeployNodeCloneVolume {
                    namespace: namespace.as_str().to_string(),
                    deploy_id: deploy_id.as_str().to_string(),
                    volume: request.volume,
                    source_namespace: request.source_namespace.as_str().to_string(),
                    source_volume: request.source_volume,
                    snapshot: request.snapshot,
                    quota: request.quota,
                    mode: request.mode,
                    owner: request.owner,
                },
            )
            .await
            .map_err(PloyzError::from)?;
        if !response.is_ok() {
            return Err(PloyzError::Deploy(DeployError::RemoteNodeError {
                operation: "deploy_node_clone_volume",
                code: response.code().to_string(),
                message: response.message().to_string(),
            }));
        }
        let Some(DaemonPayload::VolumeZfsClone(payload)) = response.payload() else {
            return Err(PloyzError::Deploy(DeployError::MissingNodePayload {
                payload: "volume zfs clone",
            }));
        };
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
        let response = self
            .client
            .clone()
            .with_policy(RpcPolicy {
                timeout: super::DEPLOY_VOLUME_CLONE_CLEANUP_RPC_TIMEOUT,
            })
            .request(
                NodeCommandSubject::deploy_clone_volume(machine_id),
                &NodeRequest::DeployNodeCleanupUncommittedVolumeClone {
                    namespace: namespace.as_str().to_string(),
                    deploy_id: deploy_id.as_str().to_string(),
                    volume: request.volume,
                    source_namespace: request.source_namespace.as_str().to_string(),
                    source_volume: request.source_volume,
                    snapshot: request.snapshot,
                },
            )
            .await
            .map_err(PloyzError::from)?;
        if response.is_ok() {
            return Ok(());
        }
        Err(PloyzError::Deploy(DeployError::RemoteNodeError {
            operation: "deploy_node_cleanup_uncommitted_volume_clone",
            code: response.code().to_string(),
            message: response.message().to_string(),
        }))
    }

    async fn drain_instance(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> ployz_error::Result<()> {
        self.expect_ok(
            NodeCommandSubject::deploy_drain_instance(machine_id),
            NodeRequest::DeployNodeDrainInstance {
                namespace: namespace.as_str().to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                instance_id: instance_id.as_str().to_string(),
            },
            "deploy_node_drain",
        )
        .await
    }

    async fn remove_instance(
        &self,
        machine_id: &MachineId,
        namespace: &Namespace,
        deploy_id: &DeployId,
        instance_id: &InstanceId,
    ) -> ployz_error::Result<()> {
        self.expect_ok(
            NodeCommandSubject::deploy_remove_instance(machine_id),
            NodeRequest::DeployNodeRemoveInstance {
                namespace: namespace.as_str().to_string(),
                deploy_id: deploy_id.as_str().to_string(),
                instance_id: instance_id.as_str().to_string(),
            },
            "deploy_node_remove",
        )
        .await
    }
}

impl NatsDeployParticipantClient {
    async fn expect_ok(
        &self,
        subject: NodeCommandSubject,
        request: NodeRequest,
        operation: &'static str,
    ) -> ployz_error::Result<()> {
        let response = self
            .client
            .request(subject, &request)
            .await
            .map_err(PloyzError::from)?;
        if response.is_ok() {
            return Ok(());
        }
        Err(PloyzError::Deploy(DeployError::RemoteNodeError {
            operation,
            code: response.code().to_string(),
            message: response.message().to_string(),
        }))
    }
}
