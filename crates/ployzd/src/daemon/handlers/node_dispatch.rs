use ployz_api::DaemonResponse;
use ployz_node_api::NodeRequest;
use tokio::sync::oneshot;

use super::super::DaemonState;

impl DaemonState {
    pub async fn handle_node_shared(&self, req: NodeRequest) -> DaemonResponse {
        match req {
            NodeRequest::Ping => self.ok("pong"),
            NodeRequest::Status => self.handle_status().await,
            NodeRequest::MeshReady { json } => self.handle_mesh_ready(json).await,
            NodeRequest::MeshSelfRecord => self.handle_mesh_self_record().await,
            NodeRequest::MachineOperationGet { id } => self.handle_machine_operation_get(&id).await,
            NodeRequest::DeployNodeInspectNamespace {
                namespace,
                deploy_id,
            } => {
                self.handle_deploy_node_inspect_namespace(&namespace, &deploy_id)
                    .await
            }
            NodeRequest::DeployNodeStartCandidate {
                namespace,
                deploy_id,
                service,
                slot_id,
                instance_id,
                spec_json,
                volumes_json,
            } => {
                self.handle_deploy_node_start_candidate(
                    &namespace,
                    &deploy_id,
                    &service,
                    &slot_id,
                    &instance_id,
                    &spec_json,
                    &volumes_json,
                )
                .await
            }
            NodeRequest::DeployNodeDrainInstance {
                namespace,
                deploy_id,
                instance_id,
            } => {
                self.handle_deploy_node_drain_instance(&namespace, &deploy_id, &instance_id)
                    .await
            }
            NodeRequest::DeployNodeRemoveInstance {
                namespace,
                deploy_id,
                instance_id,
            } => {
                self.handle_deploy_node_remove_instance(&namespace, &deploy_id, &instance_id)
                    .await
            }
            NodeRequest::DeployNodeCloneVolume {
                namespace,
                deploy_id,
                volume,
                source_namespace,
                source_volume,
                snapshot,
                quota,
                mode,
                owner,
            } => {
                self.handle_deploy_node_clone_volume(
                    &namespace,
                    &deploy_id,
                    &volume,
                    &source_namespace,
                    &source_volume,
                    &snapshot,
                    &quota,
                    &mode,
                    &owner,
                )
                .await
            }
            NodeRequest::DeployNodeCleanupUncommittedVolumeClone {
                namespace,
                deploy_id,
                volume,
                source_namespace,
                source_volume,
                snapshot,
            } => {
                self.handle_deploy_node_cleanup_uncommitted_volume_clone(
                    &namespace,
                    &deploy_id,
                    &volume,
                    &source_namespace,
                    &source_volume,
                    &snapshot,
                )
                .await
            }
            NodeRequest::VolumeZfsInspect {
                namespace,
                volume,
                machine,
            } => {
                self.handle_volume_zfs_inspect(&namespace, &volume, machine.as_deref())
                    .await
            }
            NodeRequest::VolumeZfsSnapshot {
                namespace,
                volume,
                snapshot,
            } => {
                self.handle_volume_zfs_snapshot(&namespace, &volume, &snapshot)
                    .await
            }
            NodeRequest::VolumeZfsSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                from_snapshot,
            } => {
                self.handle_volume_zfs_send(
                    &namespace,
                    &volume,
                    &snapshot,
                    &target_machine,
                    from_snapshot.as_deref(),
                )
                .await
            }
            NodeRequest::VolumeZfsPeerSnapshot {
                namespace,
                volume,
                snapshot,
            } => {
                self.handle_volume_zfs_peer_snapshot(&namespace, &volume, &snapshot)
                    .await
            }
            NodeRequest::VolumeZfsPeerSnapshotGuid {
                namespace,
                volume,
                snapshot,
            } => {
                self.handle_volume_zfs_peer_snapshot_guid(&namespace, &volume, &snapshot)
                    .await
            }
            NodeRequest::VolumeZfsPeerStartSend {
                namespace,
                volume,
                snapshot,
                target_machine,
                expected_guid,
                from_snapshot,
                from_snapshot_guid,
            } => {
                self.handle_volume_zfs_peer_start_send(
                    &namespace,
                    &volume,
                    &snapshot,
                    &target_machine,
                    expected_guid,
                    from_snapshot.as_deref(),
                    from_snapshot_guid,
                )
                .await
            }
            NodeRequest::VolumeZfsTransferGet { id } => {
                self.handle_volume_zfs_transfer_get(&id).await
            }
            NodeRequest::ImageDistribute { request } => {
                self.handle_image_distribute(&request).await
            }
            NodeRequest::ImageReceiveSession { request } => {
                self.handle_image_receive_session(&request).await
            }
            NodeRequest::ImageReceivedImport { request } => {
                self.handle_image_received_import(&request).await
            }
            NodeRequest::MeshPeerPrepareDestroy { .. }
            | NodeRequest::MeshPeerCancelDestroy { .. }
            | NodeRequest::MeshPeerExecuteDestroy { .. }
            | NodeRequest::MeshPeerPrepareUpdate { .. }
            | NodeRequest::MeshPeerExecuteUpdate { .. }
            | NodeRequest::MeshPeerRemoveMachine { .. }
            | NodeRequest::MachineTransitionSelf { .. }
            | NodeRequest::MachineStoragePromoteSelf { .. }
            | NodeRequest::MachineStorageRestoreSelf { .. } => self.err(
                "INTERNAL",
                "exclusive node request routed to shared handler",
            ),
        }
    }

    pub async fn handle_node_exclusive(
        &mut self,
        req: NodeRequest,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> DaemonResponse {
        match req {
            NodeRequest::MachineTransitionSelf { transition } => {
                self.handle_machine_transition_self(transition).await
            }
            NodeRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers,
            } => {
                self.handle_machine_storage_promote_self(replicas, &authority_peers)
                    .await
            }
            NodeRequest::MachineStorageRestoreSelf {
                participation,
                replicas,
                authority_peers,
            } => {
                self.handle_machine_storage_restore_self(participation, replicas, &authority_peers)
                    .await
            }
            NodeRequest::MeshPeerPrepareDestroy {
                operation_id,
                network_id,
                coordinator_id,
                expected_machine_ids,
            } => {
                self.handle_mesh_peer_prepare_destroy(
                    &operation_id,
                    &network_id,
                    &coordinator_id,
                    &expected_machine_ids,
                )
                .await
            }
            NodeRequest::MeshPeerCancelDestroy { operation_id } => {
                self.handle_mesh_peer_cancel_destroy(&operation_id).await
            }
            NodeRequest::MeshPeerExecuteDestroy {
                operation_id,
                network_id,
            } => {
                self.handle_mesh_peer_execute_destroy(&operation_id, &network_id, response_flushed)
                    .await
            }
            NodeRequest::MeshPeerPrepareUpdate {
                operation_id,
                version,
            } => {
                self.handle_mesh_peer_prepare_update(&operation_id, &version)
                    .await
            }
            NodeRequest::MeshPeerExecuteUpdate {
                operation_id,
                version,
            } => {
                self.handle_mesh_peer_execute_update(&operation_id, &version, response_flushed)
                    .await
            }
            NodeRequest::MeshPeerRemoveMachine {
                operation_id,
                network_id,
                machine_id,
            } => {
                self.handle_mesh_peer_remove_machine(
                    &operation_id,
                    &network_id,
                    &machine_id,
                    response_flushed,
                )
                .await
            }
            NodeRequest::Ping
            | NodeRequest::Status
            | NodeRequest::MeshReady { .. }
            | NodeRequest::MeshSelfRecord
            | NodeRequest::MachineOperationGet { .. }
            | NodeRequest::DeployNodeInspectNamespace { .. }
            | NodeRequest::DeployNodeStartCandidate { .. }
            | NodeRequest::DeployNodeDrainInstance { .. }
            | NodeRequest::DeployNodeRemoveInstance { .. }
            | NodeRequest::DeployNodeCloneVolume { .. }
            | NodeRequest::DeployNodeCleanupUncommittedVolumeClone { .. }
            | NodeRequest::VolumeZfsInspect { .. }
            | NodeRequest::VolumeZfsSnapshot { .. }
            | NodeRequest::VolumeZfsSend { .. }
            | NodeRequest::VolumeZfsPeerSnapshot { .. }
            | NodeRequest::VolumeZfsPeerSnapshotGuid { .. }
            | NodeRequest::VolumeZfsPeerStartSend { .. }
            | NodeRequest::VolumeZfsTransferGet { .. }
            | NodeRequest::ImageDistribute { .. }
            | NodeRequest::ImageReceiveSession { .. }
            | NodeRequest::ImageReceivedImport { .. } => self.err(
                "INTERNAL",
                "shared node request routed to exclusive handler",
            ),
        }
    }
}
