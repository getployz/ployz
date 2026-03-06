mod invite;
mod machine;
mod mesh;
mod service;
mod status;
mod workload;

use crate::transport::{DaemonRequest, DaemonResponse};

use super::DaemonState;

impl DaemonState {
    pub async fn handle(&mut self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::Status => self.handle_status(),
            DaemonRequest::MeshList => self.handle_mesh_list(),
            DaemonRequest::MeshStatus { network } => self.handle_mesh_status(&network),
            DaemonRequest::MeshJoin { token } => self.handle_mesh_join(&token).await,
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
            DaemonRequest::MachineAdd { target } => self.handle_machine_add(&target).await,
            DaemonRequest::MachineInviteCreate { ttl_secs } => {
                self.handle_machine_invite_create(ttl_secs).await
            }
            DaemonRequest::MachineInviteImport { token } => {
                self.handle_machine_invite_import(&token).await
            }
            DaemonRequest::MeshSelfRecord => self.handle_mesh_self_record().await,
            DaemonRequest::MeshAccept { response } => self.handle_mesh_accept(&response).await,
            DaemonRequest::WorkloadCreate { name } => self.handle_workload_create(&name).await,
            DaemonRequest::WorkloadDestroy { name } => self.handle_workload_destroy(&name).await,
            DaemonRequest::WorkloadList => self.handle_workload_list().await,
            DaemonRequest::ServiceRun { spec_json } => {
                self.handle_service_run(&spec_json).await
            }
            DaemonRequest::ServiceList => self.handle_service_list().await,
            DaemonRequest::ServiceRemove { name, namespace } => {
                self.handle_service_remove(&name, &namespace).await
            }
        }
    }
}
