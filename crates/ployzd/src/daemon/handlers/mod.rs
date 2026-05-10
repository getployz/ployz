mod build;
mod debug;
mod deploy;
mod doctor;
mod image;
mod invite;
pub(crate) mod machine;
mod mesh;
pub(crate) mod runtime;
mod status;
pub(crate) mod volume;

use ployz_api::{DaemonRequest, DaemonResponse};
use ployz_types::model::MachineId;
use tokio::sync::oneshot;

use super::DaemonState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestLane {
    Shared,
    Exclusive,
}

impl DaemonState {
    #[must_use]
    pub fn request_lane(req: &DaemonRequest) -> RequestLane {
        match req {
            DaemonRequest::DebugTick { .. }
            | DaemonRequest::MeshJoin { .. }
            | DaemonRequest::MeshBootstrap { .. }
            | DaemonRequest::MachineTransitionSelf { .. }
            | DaemonRequest::MachineStoragePromoteSelf { .. }
            | DaemonRequest::MachineStorageRestoreSelf { .. }
            | DaemonRequest::MeshInit { .. }
            | DaemonRequest::MeshStart { .. }
            | DaemonRequest::MeshStop { .. }
            | DaemonRequest::MeshDestroy { .. }
            | DaemonRequest::MeshPeerPrepareDestroy { .. }
            | DaemonRequest::MeshPeerCancelDestroy { .. }
            | DaemonRequest::MeshPeerExecuteDestroy { .. }
            | DaemonRequest::MachineUpdate { .. }
            | DaemonRequest::MachineStoragePromote { .. }
            | DaemonRequest::MeshPeerPrepareUpdate { .. }
            | DaemonRequest::MeshPeerExecuteUpdate { .. }
            | DaemonRequest::MachineRemove { .. }
            | DaemonRequest::MeshPeerRemoveMachine { .. }
            | DaemonRequest::DeployNodeCloneVolume { .. }
            | DaemonRequest::DeployNodeCleanupUncommittedVolumeClone { .. } => {
                RequestLane::Exclusive
            }
            DaemonRequest::Ping
            | DaemonRequest::Status
            | DaemonRequest::Doctor
            | DaemonRequest::DeployPreview { .. }
            | DaemonRequest::DeployApply { .. }
            | DaemonRequest::DeployExport { .. }
            | DaemonRequest::MigrateService { .. }
            | DaemonRequest::ImageStatus { .. }
            | DaemonRequest::ImageOperationGet { .. }
            | DaemonRequest::ImageOperationList
            | DaemonRequest::BuildOperationGet { .. }
            | DaemonRequest::BuildOperationList
            | DaemonRequest::DeployNodeInspectNamespace { .. }
            | DaemonRequest::DeployNodeStartCandidate { .. }
            | DaemonRequest::DeployNodeDrainInstance { .. }
            | DaemonRequest::DeployNodeRemoveInstance { .. }
            | DaemonRequest::RuntimeSubscribe
            | DaemonRequest::VolumeZfsInspect { .. }
            | DaemonRequest::VolumeZfsSnapshot { .. }
            | DaemonRequest::VolumeZfsSend { .. }
            | DaemonRequest::VolumeZfsPeerSnapshot { .. }
            | DaemonRequest::VolumeZfsPeerSnapshotGuid { .. }
            | DaemonRequest::VolumeZfsPeerStartSend { .. }
            | DaemonRequest::VolumeZfsTransferGet { .. }
            | DaemonRequest::VolumeZfsTransferList
            | DaemonRequest::MeshList
            | DaemonRequest::MeshStatus { .. }
            | DaemonRequest::MeshReady { .. }
            | DaemonRequest::MeshCreate { .. }
            | DaemonRequest::MachineList
            | DaemonRequest::MachineRtt
            | DaemonRequest::MeshPeerRttSnapshot
            | DaemonRequest::MachineInit { .. }
            | DaemonRequest::MachineAdd { .. }
            | DaemonRequest::MachineActivate { .. }
            | DaemonRequest::MachineDrain { .. }
            | DaemonRequest::MachineStandby { .. }
            | DaemonRequest::MachineOperationList
            | DaemonRequest::MachineOperationGet { .. }
            | DaemonRequest::MachineInviteCreate { .. }
            | DaemonRequest::MachineInviteRevoke { .. }
            | DaemonRequest::MachineInviteList
            | DaemonRequest::MachineInviteImport { .. }
            | DaemonRequest::AcmeChallengeReady { .. }
            | DaemonRequest::AcmeHttp01Status { .. }
            | DaemonRequest::MeshSelfRecord => RequestLane::Shared,
        }
    }

    #[must_use]
    pub fn request_lane_for_state(&self, req: &DaemonRequest) -> RequestLane {
        match req {
            DaemonRequest::MachineDrain { target }
            | DaemonRequest::MachineStandby { target, .. }
                if MachineId(target.clone()) == self.identity.machine_id =>
            {
                RequestLane::Exclusive
            }
            _ => Self::request_lane(req),
        }
    }

    pub async fn handle_shared(&self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::Ping => self.ok("pong"),
            DaemonRequest::Status => self.handle_status().await,
            DaemonRequest::Doctor => self.handle_doctor().await,
            DaemonRequest::DebugTick { .. }
            | DaemonRequest::MeshJoin { .. }
            | DaemonRequest::MeshBootstrap { .. }
            | DaemonRequest::MachineTransitionSelf { .. }
            | DaemonRequest::MachineStoragePromoteSelf { .. }
            | DaemonRequest::MachineStorageRestoreSelf { .. }
            | DaemonRequest::MeshInit { .. }
            | DaemonRequest::MeshStart { .. }
            | DaemonRequest::MeshStop { .. }
            | DaemonRequest::MeshDestroy { .. }
            | DaemonRequest::MeshPeerPrepareDestroy { .. }
            | DaemonRequest::MeshPeerCancelDestroy { .. }
            | DaemonRequest::MeshPeerExecuteDestroy { .. }
            | DaemonRequest::MachineUpdate { .. }
            | DaemonRequest::MachineStoragePromote { .. }
            | DaemonRequest::MeshPeerPrepareUpdate { .. }
            | DaemonRequest::MeshPeerExecuteUpdate { .. }
            | DaemonRequest::MachineRemove { .. }
            | DaemonRequest::MeshPeerRemoveMachine { .. } => {
                self.err("INTERNAL", "exclusive request routed to shared handler")
            }
            DaemonRequest::RuntimeSubscribe => {
                self.err("INTERNAL", "streaming request routed to shared handler")
            }
            DaemonRequest::DeployPreview {
                manifest_json,
                options,
            } => self.handle_deploy_preview(&manifest_json, &options).await,
            DaemonRequest::DeployApply {
                manifest_json,
                options,
            } => self.handle_deploy_apply(&manifest_json, &options).await,
            DaemonRequest::DeployExport { namespace } => {
                self.handle_deploy_export(&namespace).await
            }
            DaemonRequest::MigrateService { request } => self.handle_migrate_service(request).await,
            DaemonRequest::ImageStatus { request } => self.handle_image_status(&request).await,
            DaemonRequest::ImageOperationGet { id } => self.handle_image_operation_get(&id).await,
            DaemonRequest::ImageOperationList => self.handle_image_operation_list().await,
            DaemonRequest::BuildOperationGet { id } => self.handle_build_operation_get(&id).await,
            DaemonRequest::BuildOperationList => self.handle_build_operation_list().await,
            DaemonRequest::DeployNodeInspectNamespace {
                namespace,
                deploy_id,
            } => {
                self.handle_deploy_node_inspect_namespace(&namespace, &deploy_id)
                    .await
            }
            DaemonRequest::DeployNodeStartCandidate {
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
            DaemonRequest::DeployNodeDrainInstance {
                namespace,
                deploy_id,
                instance_id,
            } => {
                self.handle_deploy_node_drain_instance(&namespace, &deploy_id, &instance_id)
                    .await
            }
            DaemonRequest::DeployNodeRemoveInstance {
                namespace,
                deploy_id,
                instance_id,
            } => {
                self.handle_deploy_node_remove_instance(&namespace, &deploy_id, &instance_id)
                    .await
            }
            DaemonRequest::DeployNodeCloneVolume {
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
            DaemonRequest::DeployNodeCleanupUncommittedVolumeClone {
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
            DaemonRequest::VolumeZfsInspect {
                namespace,
                volume,
                machine,
            } => {
                self.handle_volume_zfs_inspect(&namespace, &volume, machine.as_deref())
                    .await
            }
            DaemonRequest::VolumeZfsSnapshot {
                namespace,
                volume,
                snapshot,
            } => {
                self.handle_volume_zfs_snapshot(&namespace, &volume, &snapshot)
                    .await
            }
            DaemonRequest::VolumeZfsSend {
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
            DaemonRequest::VolumeZfsPeerSnapshot {
                namespace,
                volume,
                snapshot,
            } => {
                self.handle_volume_zfs_peer_snapshot(&namespace, &volume, &snapshot)
                    .await
            }
            DaemonRequest::VolumeZfsPeerSnapshotGuid {
                namespace,
                volume,
                snapshot,
            } => {
                self.handle_volume_zfs_peer_snapshot_guid(&namespace, &volume, &snapshot)
                    .await
            }
            DaemonRequest::VolumeZfsPeerStartSend {
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
            DaemonRequest::VolumeZfsTransferGet { id } => {
                self.handle_volume_zfs_transfer_get(&id).await
            }
            DaemonRequest::VolumeZfsTransferList => self.handle_volume_zfs_transfer_list().await,
            DaemonRequest::MeshList => self.handle_mesh_list(),
            DaemonRequest::MeshStatus { network } => self.handle_mesh_status(&network),
            DaemonRequest::MeshReady { json } => self.handle_mesh_ready(json).await,
            DaemonRequest::MeshCreate { network } => self.handle_mesh_create(&network),
            DaemonRequest::MachineList => self.handle_machine_list().await,
            DaemonRequest::MachineRtt => self.handle_machine_rtt().await,
            DaemonRequest::MeshPeerRttSnapshot => self.handle_mesh_peer_rtt_snapshot().await,
            DaemonRequest::MachineInit {
                target,
                network,
                install,
            } => self.handle_machine_init(&target, &network, &install).await,
            DaemonRequest::MachineAdd { targets, options } => {
                self.handle_machine_add(&targets, &options).await
            }
            DaemonRequest::MachineActivate { target } => {
                self.handle_machine_activate(&target).await
            }
            DaemonRequest::MachineDrain { target } => {
                self.handle_remote_machine_drain(&target).await
            }
            DaemonRequest::MachineStandby { target, force } => {
                self.handle_remote_machine_standby(&target, force).await
            }
            DaemonRequest::MachineOperationList => self.handle_machine_operation_list().await,
            DaemonRequest::MachineOperationGet { id } => {
                self.handle_machine_operation_get(&id).await
            }
            DaemonRequest::MachineInviteCreate { ttl_secs } => {
                self.handle_machine_invite_create(ttl_secs).await
            }
            DaemonRequest::MachineInviteRevoke { invite_id } => {
                self.handle_machine_invite_revoke(&invite_id).await
            }
            DaemonRequest::MachineInviteList => self.handle_machine_invite_list().await,
            DaemonRequest::MachineInviteImport { token } => {
                self.handle_machine_invite_import(&token).await
            }
            DaemonRequest::AcmeChallengeReady { hostname, token } => {
                self.handle_acme_challenge_ready(&hostname, &token).await
            }
            DaemonRequest::AcmeHttp01Status { hostname } => {
                self.handle_acme_http01_status(&hostname).await
            }
            DaemonRequest::MeshSelfRecord => self.handle_mesh_self_record().await,
        }
    }

    pub async fn handle_exclusive(
        &mut self,
        req: DaemonRequest,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> DaemonResponse {
        match req {
            DaemonRequest::DebugTick { task, repeat } => self.handle_debug_tick(task, repeat).await,
            DaemonRequest::MeshJoin { token } => self.handle_mesh_join(&token).await,
            DaemonRequest::MeshBootstrap { request } => self.handle_mesh_bootstrap(&request).await,
            DaemonRequest::MachineTransitionSelf {
                goal,
                assigned_subnet,
                force,
            } => {
                self.handle_machine_transition_self(goal, assigned_subnet, force)
                    .await
            }
            DaemonRequest::MachineStoragePromoteSelf {
                replicas,
                authority_peers,
            } => {
                self.handle_machine_storage_promote_self(replicas, &authority_peers)
                    .await
            }
            DaemonRequest::MachineStorageRestoreSelf {
                participation,
                replicas,
                authority_peers,
            } => {
                self.handle_machine_storage_restore_self(participation, replicas, &authority_peers)
                    .await
            }
            DaemonRequest::MeshInit { network } => self.handle_mesh_init(&network).await,
            DaemonRequest::MeshStart { network } => self.handle_mesh_start(&network).await,
            DaemonRequest::MeshStop { force } => self.handle_mesh_stop(force).await,
            DaemonRequest::MeshDestroy { network } => self.handle_mesh_destroy(&network).await,
            DaemonRequest::MeshPeerPrepareDestroy {
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
            DaemonRequest::MeshPeerCancelDestroy { operation_id } => {
                self.handle_mesh_peer_cancel_destroy(&operation_id).await
            }
            DaemonRequest::MeshPeerExecuteDestroy {
                operation_id,
                network_id,
            } => {
                self.handle_mesh_peer_execute_destroy(&operation_id, &network_id, response_flushed)
                    .await
            }
            DaemonRequest::MachineRemove { id, force } => {
                self.handle_machine_remove(&id, force).await
            }
            DaemonRequest::MachineDrain { target } => self.handle_machine_drain(&target).await,
            DaemonRequest::MachineStandby { target, force } => {
                self.handle_machine_standby(&target, force).await
            }
            DaemonRequest::MachineUpdate { ids, version } => {
                self.handle_machine_update(&ids, &version, response_flushed)
                    .await
            }
            DaemonRequest::MachineStoragePromote { request } => {
                self.handle_machine_storage_promote(&request, response_flushed)
                    .await
            }
            DaemonRequest::MeshPeerPrepareUpdate {
                operation_id,
                version,
            } => {
                self.handle_mesh_peer_prepare_update(&operation_id, &version)
                    .await
            }
            DaemonRequest::MeshPeerExecuteUpdate {
                operation_id,
                version,
            } => {
                self.handle_mesh_peer_execute_update(&operation_id, &version, response_flushed)
                    .await
            }
            DaemonRequest::MeshPeerRemoveMachine {
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
            DaemonRequest::DeployNodeCloneVolume {
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
            DaemonRequest::DeployNodeCleanupUncommittedVolumeClone {
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
            DaemonRequest::Ping
            | DaemonRequest::Status
            | DaemonRequest::Doctor
            | DaemonRequest::DeployPreview { .. }
            | DaemonRequest::DeployApply { .. }
            | DaemonRequest::DeployExport { .. }
            | DaemonRequest::MigrateService { .. }
            | DaemonRequest::ImageStatus { .. }
            | DaemonRequest::ImageOperationGet { .. }
            | DaemonRequest::ImageOperationList
            | DaemonRequest::BuildOperationGet { .. }
            | DaemonRequest::BuildOperationList
            | DaemonRequest::DeployNodeInspectNamespace { .. }
            | DaemonRequest::DeployNodeStartCandidate { .. }
            | DaemonRequest::DeployNodeDrainInstance { .. }
            | DaemonRequest::DeployNodeRemoveInstance { .. }
            | DaemonRequest::RuntimeSubscribe
            | DaemonRequest::VolumeZfsInspect { .. }
            | DaemonRequest::VolumeZfsSnapshot { .. }
            | DaemonRequest::VolumeZfsSend { .. }
            | DaemonRequest::VolumeZfsPeerSnapshot { .. }
            | DaemonRequest::VolumeZfsPeerSnapshotGuid { .. }
            | DaemonRequest::VolumeZfsPeerStartSend { .. }
            | DaemonRequest::VolumeZfsTransferGet { .. }
            | DaemonRequest::VolumeZfsTransferList
            | DaemonRequest::MeshList
            | DaemonRequest::MeshStatus { .. }
            | DaemonRequest::MeshReady { .. }
            | DaemonRequest::MeshCreate { .. }
            | DaemonRequest::MachineList
            | DaemonRequest::MachineRtt
            | DaemonRequest::MeshPeerRttSnapshot
            | DaemonRequest::MachineInit { .. }
            | DaemonRequest::MachineAdd { .. }
            | DaemonRequest::MachineActivate { .. }
            | DaemonRequest::MachineOperationList
            | DaemonRequest::MachineOperationGet { .. }
            | DaemonRequest::MachineInviteCreate { .. }
            | DaemonRequest::MachineInviteRevoke { .. }
            | DaemonRequest::MachineInviteList
            | DaemonRequest::MachineInviteImport { .. }
            | DaemonRequest::AcmeChallengeReady { .. }
            | DaemonRequest::AcmeHttp01Status { .. }
            | DaemonRequest::MeshSelfRecord => {
                self.err("INTERNAL", "shared request routed to exclusive handler")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RequestLane;
    use crate::daemon::DaemonState;
    use ployz_api::{DaemonRequest, DebugTickTask};

    #[test]
    fn debug_tick_routes_to_exclusive_lane() {
        let lane = DaemonState::request_lane(&DaemonRequest::DebugTick {
            task: DebugTickTask::All,
            repeat: 1,
        });
        assert_eq!(lane, RequestLane::Exclusive);
    }

    #[test]
    fn machine_update_routes_to_exclusive_lane() {
        let lane = DaemonState::request_lane(&DaemonRequest::MachineUpdate {
            ids: Vec::new(),
            version: "latest".into(),
        });
        assert_eq!(lane, RequestLane::Exclusive);
    }
}
