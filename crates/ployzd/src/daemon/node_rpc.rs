mod probe;

use async_trait::async_trait;
use ployz_model::{
    ImageDistributeRequest, ImageReceiveSessionRequest, ImageReceivedImportRequest, MachineId,
};
use ployz_nats::{NatsNodeRpcClient, NodeCommandSubject, RpcFailure, RpcPolicy};
use ployz_node_api::{NodeRequest, NodeResponse};
use ployz_node_runtime::{
    DeployRpcOperation, DeployRpcTransport, ImageNodeClient, ImageNodeResponse, ImageRpcOperation,
    ImageRpcTransport, MachineLifecycleRpcOperation, MachineLifecycleRpcTransport,
    MachineOperationRpcOperation, MachineOperationRpcTransport, MachineStorageRpcOperation,
    MachineStorageRpcTransport, MachineUpdateRpcOperation, MachineUpdateRpcTransport,
    MeshReadinessRpcOperation, MeshReadinessRpcTransport, MeshRpcOperation, MeshRpcTransport,
    NodeRpcError, NodeRpcPolicy, VolumeZfsRpcOperation, VolumeZfsRpcTransport,
    decode_node_response_payload,
};

use super::DaemonState;

pub(crate) use probe::NatsNodeProbeRpcTransport;

pub(crate) const STATUS_PAYLOAD_KIND: &str = "status";
pub(crate) const MESH_READY_PAYLOAD_KIND: &str = "mesh-ready";
pub(crate) const MESH_SELF_RECORD_PAYLOAD_KIND: &str = "mesh-self-record";
pub(crate) const MACHINE_OPERATION_PAYLOAD_KIND: &str = "machine-operation";

pub(crate) fn decode_daemon_node_payload<P>(
    operation_name: &'static str,
    response: NodeResponse,
    expected_kind: &'static str,
) -> Result<P, NodeRpcError>
where
    P: serde::de::DeserializeOwned,
{
    decode_node_response_payload(operation_name, response, expected_kind)
}

#[derive(Clone)]
pub(crate) struct NatsMeshReadinessRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMeshReadinessRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MeshReadinessRpcTransport for NatsMeshReadinessRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn mesh_readiness_request(
        &self,
        machine_id: &MachineId,
        operation: MeshReadinessRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(mesh_readiness_subject(machine_id, operation), request)
            .await
            .map_err(|error| mesh_readiness_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsMachineOperationRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMachineOperationRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MachineOperationRpcTransport for NatsMachineOperationRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn machine_operation_request(
        &self,
        machine_id: &MachineId,
        operation: MachineOperationRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(machine_operation_subject(machine_id, operation), request)
            .await
            .map_err(|error| machine_operation_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsDeployRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsDeployRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DeployRpcTransport for NatsDeployRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn deploy_request(
        &self,
        machine_id: &MachineId,
        operation: DeployRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(deploy_subject(machine_id, operation), request)
            .await
            .map_err(|error| deploy_node_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsImageRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsImageRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ImageRpcTransport for NatsImageRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn image_request(
        &self,
        machine_id: &MachineId,
        operation: ImageRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(image_subject(machine_id, operation), request)
            .await
            .map_err(|error| image_node_rpc_error(operation, error))
    }
}

impl DaemonState {
    pub(crate) async fn request_image_receive_session(
        &self,
        target_machine: &MachineId,
        request: ImageReceiveSessionRequest,
    ) -> Result<ImageNodeResponse, String> {
        let client = self
            .nats_node_rpc_client()
            .await
            .map_err(|error| format!("connect node rpc for image receive session: {error}"))?;
        ImageNodeClient::new(NatsImageRpcTransport::new(client))
            .receive_session(target_machine, request)
            .await
            .map_err(|error| {
                format!("request image receive session from {target_machine}: {error}")
            })
    }

    pub(crate) async fn request_image_distribute(
        &self,
        source_machine: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<ImageNodeResponse, String> {
        let client = self
            .nats_node_rpc_client()
            .await
            .map_err(|error| format!("connect node rpc for image distribute: {error}"))?;
        ImageNodeClient::new(NatsImageRpcTransport::new(client))
            .distribute(source_machine, request)
            .await
            .map_err(|error| format!("request image distribute from {source_machine}: {error}"))
    }

    pub(crate) async fn request_image_received_import(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<ImageNodeResponse, String> {
        let client = self
            .nats_node_rpc_client()
            .await
            .map_err(|error| format!("connect node rpc for image import: {error}"))?;
        ImageNodeClient::new(NatsImageRpcTransport::new(client))
            .received_import(target_machine, request)
            .await
            .map_err(|error| format!("request image import from {target_machine}: {error}"))
    }
}

#[derive(Clone)]
pub(crate) struct NatsMachineLifecycleRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMachineLifecycleRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MachineLifecycleRpcTransport for NatsMachineLifecycleRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn machine_lifecycle_request(
        &self,
        machine_id: &MachineId,
        operation: MachineLifecycleRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(machine_lifecycle_subject(machine_id, operation), request)
            .await
            .map_err(|error| machine_lifecycle_node_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsMachineUpdateRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMachineUpdateRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MachineUpdateRpcTransport for NatsMachineUpdateRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn machine_update_request(
        &self,
        machine_id: &MachineId,
        operation: MachineUpdateRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(machine_update_subject(machine_id, operation), request)
            .await
            .map_err(|error| machine_update_node_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsMachineStorageRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMachineStorageRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MachineStorageRpcTransport for NatsMachineStorageRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn machine_request(
        &self,
        machine_id: &MachineId,
        operation: MachineStorageRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(machine_storage_subject(machine_id, operation), request)
            .await
            .map_err(|error| machine_storage_node_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsMeshRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsMeshRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl MeshRpcTransport for NatsMeshRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn mesh_request(
        &self,
        machine_id: &MachineId,
        operation: MeshRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(mesh_subject(machine_id, operation), request)
            .await
            .map_err(|error| mesh_node_rpc_error(operation, error))
    }
}

#[derive(Clone)]
pub(crate) struct NatsVolumeZfsRpcTransport {
    client: NatsNodeRpcClient,
}

impl NatsVolumeZfsRpcTransport {
    #[must_use]
    pub(crate) fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl VolumeZfsRpcTransport for NatsVolumeZfsRpcTransport {
    fn with_node_rpc_policy(&self, policy: NodeRpcPolicy) -> Self {
        Self {
            client: self.client.clone().with_policy(RpcPolicy {
                timeout: policy.timeout,
            }),
        }
    }

    async fn volume_zfs_request(
        &self,
        machine_id: &MachineId,
        operation: VolumeZfsRpcOperation,
        request: &NodeRequest,
    ) -> Result<NodeResponse, NodeRpcError> {
        self.client
            .request_node_response(volume_zfs_subject(machine_id, operation), request)
            .await
            .map_err(|error| node_rpc_error(operation, error))
    }
}

fn mesh_readiness_subject(
    machine_id: &MachineId,
    operation: MeshReadinessRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MeshReadinessRpcOperation::Ready => NodeCommandSubject::mesh_ready(machine_id),
        MeshReadinessRpcOperation::SelfRecord => NodeCommandSubject::mesh_self_record(machine_id),
    }
}

fn machine_operation_subject(
    machine_id: &MachineId,
    operation: MachineOperationRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MachineOperationRpcOperation::Get => NodeCommandSubject::machine_operation_get(machine_id),
    }
}

fn deploy_subject(machine_id: &MachineId, operation: DeployRpcOperation) -> NodeCommandSubject {
    match operation {
        DeployRpcOperation::InspectNamespace => {
            NodeCommandSubject::deploy_inspect_namespace(machine_id)
        }
        DeployRpcOperation::StartCandidate => {
            NodeCommandSubject::deploy_start_candidate(machine_id)
        }
        DeployRpcOperation::CloneVolume | DeployRpcOperation::CleanupVolumeClone => {
            NodeCommandSubject::deploy_clone_volume(machine_id)
        }
        DeployRpcOperation::DrainInstance => NodeCommandSubject::deploy_drain_instance(machine_id),
        DeployRpcOperation::RemoveInstance => {
            NodeCommandSubject::deploy_remove_instance(machine_id)
        }
    }
}

fn image_subject(machine_id: &MachineId, operation: ImageRpcOperation) -> NodeCommandSubject {
    match operation {
        ImageRpcOperation::ReceiveSession => NodeCommandSubject::image_receive_session(machine_id),
        ImageRpcOperation::Distribute => NodeCommandSubject::image_distribute(machine_id),
        ImageRpcOperation::ReceivedImport => NodeCommandSubject::image_received_import(machine_id),
    }
}

fn machine_storage_subject(
    machine_id: &MachineId,
    operation: MachineStorageRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MachineStorageRpcOperation::StoragePromoteSelf => {
            NodeCommandSubject::machine_storage_promote_self(machine_id)
        }
        MachineStorageRpcOperation::StorageRestoreSelf => {
            NodeCommandSubject::machine_storage_restore_self(machine_id)
        }
    }
}

fn machine_lifecycle_subject(
    machine_id: &MachineId,
    operation: MachineLifecycleRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MachineLifecycleRpcOperation::TransitionSelf => {
            NodeCommandSubject::machine_transition_self(machine_id)
        }
    }
}

fn machine_update_subject(
    machine_id: &MachineId,
    operation: MachineUpdateRpcOperation,
) -> NodeCommandSubject {
    match operation {
        MachineUpdateRpcOperation::PrepareUpdate => {
            NodeCommandSubject::machine_update_prepare(machine_id)
        }
        MachineUpdateRpcOperation::ExecuteUpdate => {
            NodeCommandSubject::machine_update_execute(machine_id)
        }
    }
}

fn mesh_subject(machine_id: &MachineId, operation: MeshRpcOperation) -> NodeCommandSubject {
    match operation {
        MeshRpcOperation::PrepareDestroy => NodeCommandSubject::mesh_prepare_destroy(machine_id),
        MeshRpcOperation::CancelDestroy => NodeCommandSubject::mesh_cancel_destroy(machine_id),
        MeshRpcOperation::ExecuteDestroy => NodeCommandSubject::mesh_execute_destroy(machine_id),
        MeshRpcOperation::RemoveMachine => NodeCommandSubject::mesh_remove_machine(machine_id),
    }
}

fn volume_zfs_subject(
    machine_id: &MachineId,
    operation: VolumeZfsRpcOperation,
) -> NodeCommandSubject {
    match operation {
        VolumeZfsRpcOperation::Inspect => NodeCommandSubject::volume_zfs_inspect(machine_id),
        VolumeZfsRpcOperation::Snapshot | VolumeZfsRpcOperation::PeerSnapshot => {
            NodeCommandSubject::volume_zfs_snapshot(machine_id)
        }
        VolumeZfsRpcOperation::Send => NodeCommandSubject::volume_zfs_send(machine_id),
        VolumeZfsRpcOperation::TransferGet => {
            NodeCommandSubject::volume_zfs_transfer_get(machine_id)
        }
        VolumeZfsRpcOperation::PeerSnapshotGuid => {
            NodeCommandSubject::volume_zfs_snapshot_guid(machine_id)
        }
        VolumeZfsRpcOperation::PeerStartSend => {
            NodeCommandSubject::volume_zfs_start_send(machine_id)
        }
    }
}

fn deploy_node_rpc_error(operation: DeployRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn image_node_rpc_error(operation: ImageRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn mesh_readiness_rpc_error(
    operation: MeshReadinessRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn machine_operation_rpc_error(
    operation: MachineOperationRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn machine_lifecycle_node_rpc_error(
    operation: MachineLifecycleRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn machine_update_node_rpc_error(
    operation: MachineUpdateRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn machine_storage_node_rpc_error(
    operation: MachineStorageRpcOperation,
    error: RpcFailure,
) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn mesh_node_rpc_error(operation: MeshRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

fn node_rpc_error(operation: VolumeZfsRpcOperation, error: RpcFailure) -> NodeRpcError {
    NodeRpcError::new(operation.operation_name(), error.code(), error.message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_nats::RpcFailureKind;

    #[test]
    fn deploy_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                DeployRpcOperation::InspectNamespace,
                NodeCommandSubject::deploy_inspect_namespace(&machine_id),
            ),
            (
                DeployRpcOperation::StartCandidate,
                NodeCommandSubject::deploy_start_candidate(&machine_id),
            ),
            (
                DeployRpcOperation::CloneVolume,
                NodeCommandSubject::deploy_clone_volume(&machine_id),
            ),
            (
                DeployRpcOperation::CleanupVolumeClone,
                NodeCommandSubject::deploy_clone_volume(&machine_id),
            ),
            (
                DeployRpcOperation::DrainInstance,
                NodeCommandSubject::deploy_drain_instance(&machine_id),
            ),
            (
                DeployRpcOperation::RemoveInstance,
                NodeCommandSubject::deploy_remove_instance(&machine_id),
            ),
        ] {
            assert_eq!(deploy_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn image_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                ImageRpcOperation::ReceiveSession,
                NodeCommandSubject::image_receive_session(&machine_id),
            ),
            (
                ImageRpcOperation::Distribute,
                NodeCommandSubject::image_distribute(&machine_id),
            ),
            (
                ImageRpcOperation::ReceivedImport,
                NodeCommandSubject::image_received_import(&machine_id),
            ),
        ] {
            assert_eq!(image_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn mesh_readiness_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                MeshReadinessRpcOperation::Ready,
                NodeCommandSubject::mesh_ready(&machine_id),
            ),
            (
                MeshReadinessRpcOperation::SelfRecord,
                NodeCommandSubject::mesh_self_record(&machine_id),
            ),
        ] {
            assert_eq!(mesh_readiness_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn machine_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [(
            MachineOperationRpcOperation::Get,
            NodeCommandSubject::machine_operation_get(&machine_id),
        )] {
            assert_eq!(machine_operation_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn mesh_readiness_rpc_transport_error_maps_operation_and_failure_context() {
        let error = mesh_readiness_rpc_error(
            MeshReadinessRpcOperation::Ready,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "mesh_ready");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn machine_operation_rpc_transport_error_maps_operation_and_failure_context() {
        let error = machine_operation_rpc_error(
            MachineOperationRpcOperation::Get,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "machine_operation_get");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn machine_lifecycle_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [(
            MachineLifecycleRpcOperation::TransitionSelf,
            NodeCommandSubject::machine_transition_self(&machine_id),
        )] {
            assert_eq!(machine_lifecycle_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn machine_update_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                MachineUpdateRpcOperation::PrepareUpdate,
                NodeCommandSubject::machine_update_prepare(&machine_id),
            ),
            (
                MachineUpdateRpcOperation::ExecuteUpdate,
                NodeCommandSubject::machine_update_execute(&machine_id),
            ),
        ] {
            assert_eq!(machine_update_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn machine_lifecycle_rpc_transport_error_maps_operation_and_failure_context() {
        let error = machine_lifecycle_node_rpc_error(
            MachineLifecycleRpcOperation::TransitionSelf,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "machine_transition_self");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn machine_update_rpc_transport_error_maps_operation_and_failure_context() {
        let error = machine_update_node_rpc_error(
            MachineUpdateRpcOperation::ExecuteUpdate,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "machine_update_execute");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn mesh_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                MeshRpcOperation::PrepareDestroy,
                NodeCommandSubject::mesh_prepare_destroy(&machine_id),
            ),
            (
                MeshRpcOperation::CancelDestroy,
                NodeCommandSubject::mesh_cancel_destroy(&machine_id),
            ),
            (
                MeshRpcOperation::ExecuteDestroy,
                NodeCommandSubject::mesh_execute_destroy(&machine_id),
            ),
            (
                MeshRpcOperation::RemoveMachine,
                NodeCommandSubject::mesh_remove_machine(&machine_id),
            ),
        ] {
            assert_eq!(mesh_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn machine_storage_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                MachineStorageRpcOperation::StoragePromoteSelf,
                NodeCommandSubject::machine_storage_promote_self(&machine_id),
            ),
            (
                MachineStorageRpcOperation::StorageRestoreSelf,
                NodeCommandSubject::machine_storage_restore_self(&machine_id),
            ),
        ] {
            assert_eq!(machine_storage_subject(&machine_id, operation), expected);
        }
    }

    #[test]
    fn machine_storage_rpc_transport_error_maps_operation_and_failure_context() {
        let error = machine_storage_node_rpc_error(
            MachineStorageRpcOperation::StoragePromoteSelf,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "machine_storage_promote_self");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn mesh_rpc_transport_error_maps_operation_and_failure_context() {
        let error = mesh_node_rpc_error(
            MeshRpcOperation::ExecuteDestroy,
            RpcFailure::new(RpcFailureKind::NoResponders, "no subscribers"),
        );
        assert_eq!(error.operation, "mesh_peer_execute_destroy");
        assert_eq!(error.code, "NATS_RPC_NO_RESPONDERS");
        assert_eq!(error.message, "no subscribers");
    }

    #[test]
    fn volume_zfs_operation_maps_to_expected_nats_subject_constructor() {
        let machine_id = MachineId::new("machine-a");

        for (operation, expected) in [
            (
                VolumeZfsRpcOperation::Inspect,
                NodeCommandSubject::volume_zfs_inspect(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::Snapshot,
                NodeCommandSubject::volume_zfs_snapshot(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::Send,
                NodeCommandSubject::volume_zfs_send(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::TransferGet,
                NodeCommandSubject::volume_zfs_transfer_get(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::PeerSnapshot,
                NodeCommandSubject::volume_zfs_snapshot(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::PeerSnapshotGuid,
                NodeCommandSubject::volume_zfs_snapshot_guid(&machine_id),
            ),
            (
                VolumeZfsRpcOperation::PeerStartSend,
                NodeCommandSubject::volume_zfs_start_send(&machine_id),
            ),
        ] {
            assert_eq!(volume_zfs_subject(&machine_id, operation), expected);
        }
    }
}
