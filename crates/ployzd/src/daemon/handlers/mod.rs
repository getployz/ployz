mod coordination;
mod debug;
mod deploy;
mod doctor;
mod invite;
pub(crate) mod machine;
mod mesh;
mod status;

use ployz_api::{DaemonRequest, DaemonResponse};

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
            | DaemonRequest::MeshPromote { .. }
            | DaemonRequest::MeshStandby { .. }
            | DaemonRequest::MeshSetParticipation { .. }
            | DaemonRequest::MeshInit { .. }
            | DaemonRequest::MeshUp { .. }
            | DaemonRequest::MeshDown
            | DaemonRequest::MeshDestroy { .. } => RequestLane::Exclusive,
            DaemonRequest::Status
            | DaemonRequest::Doctor
            | DaemonRequest::DeployPreview { .. }
            | DaemonRequest::DeployApply { .. }
            | DaemonRequest::DeployExport { .. }
            | DaemonRequest::MeshList
            | DaemonRequest::MeshStatus { .. }
            | DaemonRequest::MeshReady { .. }
            | DaemonRequest::MeshCreate { .. }
            | DaemonRequest::MachineList
            | DaemonRequest::MachineInit { .. }
            | DaemonRequest::MachineAdd { .. }
            | DaemonRequest::MachineEnable { .. }
            | DaemonRequest::MachineDrain { .. }
            | DaemonRequest::MachineDisable { .. }
            | DaemonRequest::MachineRemove { .. }
            | DaemonRequest::MachineOperationList
            | DaemonRequest::MachineOperationGet { .. }
            | DaemonRequest::MachineInviteCreate { .. }
            | DaemonRequest::MachineInviteRevoke { .. }
            | DaemonRequest::MachineInviteList
            | DaemonRequest::MachineInviteImport { .. }
            | DaemonRequest::Coord { .. }
            | DaemonRequest::MeshSelfRecord
            | DaemonRequest::MeshAccept { .. } => RequestLane::Shared,
        }
    }

    pub async fn handle_shared(&self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::Status => self.handle_status(),
            DaemonRequest::Doctor => self.handle_doctor().await,
            DaemonRequest::DebugTick { .. }
            | DaemonRequest::MeshJoin { .. }
            | DaemonRequest::MeshBootstrap { .. }
            | DaemonRequest::MeshPromote { .. }
            | DaemonRequest::MeshStandby { .. }
            | DaemonRequest::MeshSetParticipation { .. }
            | DaemonRequest::MeshInit { .. }
            | DaemonRequest::MeshUp { .. }
            | DaemonRequest::MeshDown
            | DaemonRequest::MeshDestroy { .. } => {
                self.err("INTERNAL", "exclusive request routed to shared handler")
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
            DaemonRequest::MeshList => self.handle_mesh_list(),
            DaemonRequest::MeshStatus { network } => self.handle_mesh_status(&network),
            DaemonRequest::MeshReady { json } => self.handle_mesh_ready(json).await,
            DaemonRequest::MeshCreate { network } => self.handle_mesh_create(&network),
            DaemonRequest::MachineList => self.handle_machine_list().await,
            DaemonRequest::MachineInit {
                target,
                network,
                install,
            } => self.handle_machine_init(&target, &network, &install).await,
            DaemonRequest::MachineAdd { targets, options } => {
                self.handle_machine_add(&targets, &options).await
            }
            DaemonRequest::MachineEnable { target } => self.handle_machine_enable(&target).await,
            DaemonRequest::MachineDrain { target } => self.handle_machine_drain(&target).await,
            DaemonRequest::MachineDisable { target, force } => {
                self.handle_machine_disable(&target, force).await
            }
            DaemonRequest::MachineRemove { id, force } => {
                self.handle_machine_remove(&id, force).await
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
            DaemonRequest::Coord { op } => self.handle_coord(op).await,
            DaemonRequest::MeshSelfRecord => self.handle_mesh_self_record().await,
            DaemonRequest::MeshAccept { response } => self.handle_mesh_accept(&response).await,
        }
    }

    pub async fn handle_exclusive(&mut self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::DebugTick { task, repeat } => self.handle_debug_tick(task, repeat).await,
            DaemonRequest::MeshJoin { token } => self.handle_mesh_join(&token).await,
            DaemonRequest::MeshBootstrap { request } => self.handle_mesh_bootstrap(&request).await,
            DaemonRequest::MeshPromote { assigned_subnet } => {
                self.handle_mesh_promote(assigned_subnet).await
            }
            DaemonRequest::MeshStandby { force } => self.handle_mesh_standby(force).await,
            DaemonRequest::MeshSetParticipation { participation } => {
                self.handle_mesh_set_participation(participation).await
            }
            DaemonRequest::MeshInit { network } => self.handle_mesh_init(&network).await,
            DaemonRequest::MeshUp {
                network,
                skip_bootstrap_wait,
            } => self.handle_mesh_up(&network, skip_bootstrap_wait).await,
            DaemonRequest::MeshDown => self.handle_mesh_down().await,
            DaemonRequest::MeshDestroy { network } => self.handle_mesh_destroy(&network).await,
            DaemonRequest::Status
            | DaemonRequest::Doctor
            | DaemonRequest::DeployPreview { .. }
            | DaemonRequest::DeployApply { .. }
            | DaemonRequest::DeployExport { .. }
            | DaemonRequest::MeshList
            | DaemonRequest::MeshStatus { .. }
            | DaemonRequest::MeshReady { .. }
            | DaemonRequest::MeshCreate { .. }
            | DaemonRequest::MachineList
            | DaemonRequest::MachineInit { .. }
            | DaemonRequest::MachineAdd { .. }
            | DaemonRequest::MachineEnable { .. }
            | DaemonRequest::MachineDrain { .. }
            | DaemonRequest::MachineDisable { .. }
            | DaemonRequest::MachineRemove { .. }
            | DaemonRequest::MachineOperationList
            | DaemonRequest::MachineOperationGet { .. }
            | DaemonRequest::MachineInviteCreate { .. }
            | DaemonRequest::MachineInviteRevoke { .. }
            | DaemonRequest::MachineInviteList
            | DaemonRequest::MachineInviteImport { .. }
            | DaemonRequest::Coord { .. }
            | DaemonRequest::MeshSelfRecord
            | DaemonRequest::MeshAccept { .. } => {
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
}
