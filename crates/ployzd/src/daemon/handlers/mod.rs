mod deploy;
mod invite;
mod machine;
mod mesh;
mod status;

use ployz_sdk::transport::{DaemonRequest, DaemonResponse};

use super::DaemonState;

impl DaemonState {
    pub async fn handle(&mut self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::Status => self.handle_status(),
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
            DaemonRequest::MeshJoin { token } => self.handle_mesh_join(&token).await,
            DaemonRequest::MeshReady { json } => self.handle_mesh_ready(json).await,
            DaemonRequest::MeshCreate { network } => self.handle_mesh_create(&network),
            DaemonRequest::MeshInit { network } => self.handle_mesh_init(&network).await,
            DaemonRequest::MeshUp {
                network,
                skip_bootstrap_wait,
            } => self.handle_mesh_up(&network, skip_bootstrap_wait).await,
            DaemonRequest::MeshDown => self.handle_mesh_down().await,
            DaemonRequest::MeshDestroy { network } => self.handle_mesh_destroy(&network).await,
            DaemonRequest::MachineList => self.handle_machine_list().await,
            DaemonRequest::MachineInit { target, network } => {
                self.handle_machine_init(&target, &network).await
            }
            DaemonRequest::MachineAdd { targets } => self.handle_machine_add(&targets).await,
            DaemonRequest::MachineDrain { id } => self.handle_machine_drain(&id).await,
            DaemonRequest::MachineRemove { id, force } => {
                self.handle_machine_remove(&id, force).await
            }
            DaemonRequest::MachineInviteCreate { ttl_secs } => {
                self.handle_machine_invite_create(ttl_secs).await
            }
            DaemonRequest::MachineInviteImport { token } => {
                self.handle_machine_invite_import(&token).await
            }
            DaemonRequest::MeshSelfRecord => self.handle_mesh_self_record().await,
            DaemonRequest::MeshAccept { response } => self.handle_mesh_accept(&response).await,
            DaemonRequest::MachineLabel { id, set, remove } => {
                self.handle_machine_label(&id, &set, &remove).await
            }
        }
    }
}
