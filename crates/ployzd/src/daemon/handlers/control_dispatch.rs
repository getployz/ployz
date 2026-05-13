use crate::ipc::listener::IncomingRequest;
use ployz_api::{DaemonRequest, DaemonResponse};
use tokio::sync::oneshot;

use super::super::DaemonState;

impl DaemonState {
    pub async fn handle_command_shared(&self, req: IncomingRequest) -> DaemonResponse {
        match req {
            IncomingRequest::Control(request) => self.handle_shared(request).await,
            IncomingRequest::Node(request) => self.handle_node_shared(request).await,
        }
    }

    pub async fn handle_command_exclusive(
        &mut self,
        req: IncomingRequest,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> DaemonResponse {
        match req {
            IncomingRequest::Control(request) => {
                self.handle_exclusive(request, response_flushed).await
            }
            IncomingRequest::Node(request) => {
                self.handle_node_exclusive(request, response_flushed).await
            }
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
            | DaemonRequest::MeshInit { .. }
            | DaemonRequest::MeshStart { .. }
            | DaemonRequest::MeshStop { .. }
            | DaemonRequest::MeshDestroy { .. }
            | DaemonRequest::MachineUpdate { .. }
            | DaemonRequest::MachineStoragePromote { .. }
            | DaemonRequest::MachineRemove { .. } => {
                self.err("INTERNAL", "exclusive request routed to shared handler")
            }
            DaemonRequest::RuntimeSubscribe => {
                self.err("INTERNAL", "streaming request routed to shared handler")
            }
            DaemonRequest::DeployPreview {
                manifest_json,
                options,
            } => self.handle_deploy_preview(&manifest_json, &options).await,
            DaemonRequest::DeployPrepare { manifest_json } => {
                self.handle_deploy_prepare(&manifest_json).await
            }
            DaemonRequest::DeployApply {
                manifest_json,
                options,
            } => self.handle_deploy_apply(&manifest_json, &options).await,
            DaemonRequest::DeployApplyPrepared { request } => {
                self.handle_deploy_apply_prepared(&request).await
            }
            DaemonRequest::DeployExport { namespace } => {
                self.handle_deploy_export(&namespace).await
            }
            DaemonRequest::BranchNamespace { request } => {
                self.handle_branch_namespace(request).await
            }
            DaemonRequest::BranchApplyPrepared { request } => {
                self.handle_branch_apply_prepared(&request).await
            }
            DaemonRequest::BranchEnvironmentStatus { request } => {
                self.handle_branch_environment_status(&request).await
            }
            DaemonRequest::BranchEnvironmentList => self.handle_branch_environment_list().await,
            DaemonRequest::MigrateService { request } => self.handle_migrate_service(request).await,
            DaemonRequest::ImageStatus { request } => self.handle_image_status(&request).await,
            DaemonRequest::ImageInspect { request } => self.handle_image_inspect(&request).await,
            DaemonRequest::ImagePush { request } => self.handle_image_push(&request).await,
            DaemonRequest::ImageDistribute { request } => {
                self.handle_image_distribute(&request).await
            }
            DaemonRequest::ImageReceiveSession { request } => {
                self.handle_image_receive_session(&request).await
            }
            DaemonRequest::ImageReceivedImport { request } => {
                self.handle_image_received_import(&request).await
            }
            DaemonRequest::ImageOperationGet { id } => self.handle_image_operation_get(&id).await,
            DaemonRequest::ImageOperationList => self.handle_image_operation_list().await,
            DaemonRequest::BuildLocal { request } => self.handle_build_local(&request).await,
            DaemonRequest::BuildMachine { request } => self.handle_build_machine(&request).await,
            DaemonRequest::BuildOperationGet { id } => self.handle_build_operation_get(&id).await,
            DaemonRequest::BuildOperationList => self.handle_build_operation_list().await,
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
            DaemonRequest::MeshInit { network } => self.handle_mesh_init(&network).await,
            DaemonRequest::MeshStart { network } => self.handle_mesh_start(&network).await,
            DaemonRequest::MeshStop { force } => self.handle_mesh_stop(force).await,
            DaemonRequest::MeshDestroy { network } => self.handle_mesh_destroy(&network).await,
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
            DaemonRequest::Ping
            | DaemonRequest::Status
            | DaemonRequest::Doctor
            | DaemonRequest::DeployPreview { .. }
            | DaemonRequest::DeployPrepare { .. }
            | DaemonRequest::DeployApply { .. }
            | DaemonRequest::DeployApplyPrepared { .. }
            | DaemonRequest::DeployExport { .. }
            | DaemonRequest::BranchNamespace { .. }
            | DaemonRequest::BranchApplyPrepared { .. }
            | DaemonRequest::BranchEnvironmentStatus { .. }
            | DaemonRequest::BranchEnvironmentList
            | DaemonRequest::MigrateService { .. }
            | DaemonRequest::ImageStatus { .. }
            | DaemonRequest::ImageInspect { .. }
            | DaemonRequest::ImagePush { .. }
            | DaemonRequest::ImageDistribute { .. }
            | DaemonRequest::ImageReceiveSession { .. }
            | DaemonRequest::ImageReceivedImport { .. }
            | DaemonRequest::ImageOperationGet { .. }
            | DaemonRequest::ImageOperationList
            | DaemonRequest::BuildLocal { .. }
            | DaemonRequest::BuildMachine { .. }
            | DaemonRequest::BuildOperationGet { .. }
            | DaemonRequest::BuildOperationList
            | DaemonRequest::RuntimeSubscribe
            | DaemonRequest::VolumeZfsInspect { .. }
            | DaemonRequest::VolumeZfsSnapshot { .. }
            | DaemonRequest::VolumeZfsSend { .. }
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
