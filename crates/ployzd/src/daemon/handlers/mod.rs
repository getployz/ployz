pub(crate) mod coordination;
mod debug;
mod deploy;
mod doctor;
mod invite;
pub(crate) mod machine;
mod mesh;
mod node_status;
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
            | DaemonRequest::MeshReady
            | DaemonRequest::NodeStatus
            | DaemonRequest::MeshCreate { .. }
            | DaemonRequest::MachineList
            | DaemonRequest::MachineInit { .. }
            | DaemonRequest::MachineAdd { .. }
            | DaemonRequest::MachineRemove { .. }
            | DaemonRequest::MachineDrain { .. }
            | DaemonRequest::MachineUndrain { .. }
            | DaemonRequest::MachineOperationList
            | DaemonRequest::MachineOperationGet { .. }
            | DaemonRequest::MachineInviteCreate { .. }
            | DaemonRequest::MachineInviteImport { .. }
            | DaemonRequest::MeshSelfRecord
            | DaemonRequest::MeshAccept { .. }
            | DaemonRequest::CoordinationPrepare { .. }
            | DaemonRequest::CoordinationRenew { .. }
            | DaemonRequest::CoordinationCommit { .. }
            | DaemonRequest::CoordinationAbort { .. } => RequestLane::Shared,
        }
    }

    pub async fn handle_shared(&self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::Status => self.handle_status(),
            DaemonRequest::Doctor => self.handle_doctor().await,
            DaemonRequest::DebugTick { .. }
            | DaemonRequest::MeshJoin { .. }
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
            DaemonRequest::MeshReady => self.handle_mesh_ready().await,
            DaemonRequest::NodeStatus => self.handle_node_status().await,
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
            DaemonRequest::MachineRemove { id, force } => {
                self.handle_machine_remove(&id, force).await
            }
            DaemonRequest::MachineDrain { id } => self.handle_machine_drain(&id, true).await,
            DaemonRequest::MachineUndrain { id } => self.handle_machine_drain(&id, false).await,
            DaemonRequest::MachineOperationList => self.handle_machine_operation_list().await,
            DaemonRequest::MachineOperationGet { id } => {
                self.handle_machine_operation_get(&id).await
            }
            DaemonRequest::MachineInviteCreate { ttl_secs } => {
                self.handle_machine_invite_create(ttl_secs).await
            }
            DaemonRequest::MachineInviteImport { token } => {
                self.handle_machine_invite_import(&token).await
            }
            DaemonRequest::MeshSelfRecord => self.handle_mesh_self_record().await,
            DaemonRequest::MeshAccept { response } => self.handle_mesh_accept(&response).await,
            DaemonRequest::CoordinationPrepare { request } => {
                self.handle_coordination_prepare(request).await
            }
            DaemonRequest::CoordinationRenew { request } => {
                self.handle_coordination_renew(request).await
            }
            DaemonRequest::CoordinationCommit { request } => {
                self.handle_coordination_commit(request).await
            }
            DaemonRequest::CoordinationAbort { request } => {
                self.handle_coordination_abort(request).await
            }
        }
    }

    pub async fn handle_exclusive(&mut self, req: DaemonRequest) -> DaemonResponse {
        match req {
            DaemonRequest::DebugTick { task, repeat } => self.handle_debug_tick(task, repeat).await,
            DaemonRequest::MeshJoin { token } => self.handle_mesh_join(&token).await,
            DaemonRequest::MeshInit { network } => self.handle_mesh_init(&network).await,
            DaemonRequest::MeshUp {
                network,
                allow_disconnected_bootstrap,
            } => {
                self.handle_mesh_up(&network, allow_disconnected_bootstrap)
                    .await
            }
            DaemonRequest::MeshDown => self.handle_mesh_down().await,
            DaemonRequest::MeshDestroy { network } => self.handle_mesh_destroy(&network).await,
            DaemonRequest::Status
            | DaemonRequest::Doctor
            | DaemonRequest::DeployPreview { .. }
            | DaemonRequest::DeployApply { .. }
            | DaemonRequest::DeployExport { .. }
            | DaemonRequest::MeshList
            | DaemonRequest::MeshStatus { .. }
            | DaemonRequest::MeshReady
            | DaemonRequest::NodeStatus
            | DaemonRequest::MeshCreate { .. }
            | DaemonRequest::MachineList
            | DaemonRequest::MachineInit { .. }
            | DaemonRequest::MachineAdd { .. }
            | DaemonRequest::MachineRemove { .. }
            | DaemonRequest::MachineDrain { .. }
            | DaemonRequest::MachineUndrain { .. }
            | DaemonRequest::MachineOperationList
            | DaemonRequest::MachineOperationGet { .. }
            | DaemonRequest::MachineInviteCreate { .. }
            | DaemonRequest::MachineInviteImport { .. }
            | DaemonRequest::MeshSelfRecord
            | DaemonRequest::MeshAccept { .. }
            | DaemonRequest::CoordinationPrepare { .. }
            | DaemonRequest::CoordinationRenew { .. }
            | DaemonRequest::CoordinationCommit { .. }
            | DaemonRequest::CoordinationAbort { .. } => {
                self.err("INTERNAL", "shared request routed to exclusive handler")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RequestLane;
    use crate::daemon::DaemonState;
    use ployz_api::{
        CoordinationAbortRequest, CoordinationCommitRequest, CoordinationOperation,
        CoordinationPrepareRequest, CoordinationRenewRequest, DaemonRequest, DebugTickTask,
        DeployOptions, MachineAddOptions, MachineInstallOptions,
    };

    #[test]
    fn debug_tick_routes_to_exclusive_lane() {
        let lane = DaemonState::request_lane(&DaemonRequest::DebugTick {
            task: DebugTickTask::All,
            repeat: 1,
        });
        assert_eq!(lane, RequestLane::Exclusive);
    }

    #[test]
    fn shared_requests_route_to_shared_lane() {
        let requests = [
            DaemonRequest::Status,
            DaemonRequest::Doctor,
            DaemonRequest::DeployPreview {
                manifest_json: "{}".into(),
                options: DeployOptions::default(),
            },
            DaemonRequest::DeployApply {
                manifest_json: "{}".into(),
                options: DeployOptions::default(),
            },
            DaemonRequest::DeployExport {
                namespace: "prod".into(),
            },
            DaemonRequest::MeshList,
            DaemonRequest::MeshStatus {
                network: "alpha".into(),
            },
            DaemonRequest::MeshReady,
            DaemonRequest::NodeStatus,
            DaemonRequest::MeshCreate {
                network: "alpha".into(),
            },
            DaemonRequest::MachineList,
            DaemonRequest::MachineInit {
                target: "root@host".into(),
                network: "alpha".into(),
                install: MachineInstallOptions::default(),
            },
            DaemonRequest::MachineAdd {
                targets: vec!["root@host".into()],
                options: MachineAddOptions::default(),
            },
            DaemonRequest::MachineRemove {
                id: "machine-1".into(),
                force: false,
            },
            DaemonRequest::MachineOperationList,
            DaemonRequest::MachineOperationGet { id: "op-1".into() },
            DaemonRequest::MachineInviteCreate { ttl_secs: 300 },
            DaemonRequest::MachineInviteImport {
                token: "token".into(),
            },
            DaemonRequest::MeshSelfRecord,
            DaemonRequest::MeshAccept {
                response: "ok".into(),
            },
            DaemonRequest::CoordinationPrepare {
                request: CoordinationPrepareRequest {
                    owner_id: "founder-a".into(),
                    nonce: "n1".into(),
                    lease_ttl_secs: 30,
                    operation: CoordinationOperation::SubnetClaimPrepare {
                        machine_id: "m1".into(),
                        subnet: "10.210.1.0/24".into(),
                    },
                },
            },
            DaemonRequest::CoordinationRenew {
                request: CoordinationRenewRequest {
                    owner_id: "founder-a".into(),
                    nonce: "n1".into(),
                    lease_ttl_secs: 30,
                    operation: CoordinationOperation::SubnetClaimPrepare {
                        machine_id: "m1".into(),
                        subnet: "10.210.1.0/24".into(),
                    },
                },
            },
            DaemonRequest::CoordinationCommit {
                request: CoordinationCommitRequest {
                    owner_id: "founder-a".into(),
                    nonce: "n1".into(),
                    prepare_tokens: vec!["token".into()],
                    operation: CoordinationOperation::SubnetClaimCommit {
                        machine_id: "m1".into(),
                        subnet: "10.210.1.0/24".into(),
                    },
                },
            },
            DaemonRequest::CoordinationAbort {
                request: CoordinationAbortRequest {
                    owner_id: "founder-a".into(),
                    nonce: "n1".into(),
                    operation: CoordinationOperation::SubnetClaimAbort {
                        machine_id: "m1".into(),
                        subnet: "10.210.1.0/24".into(),
                    },
                },
            },
        ];

        for request in requests {
            assert_eq!(DaemonState::request_lane(&request), RequestLane::Shared);
        }
    }

    #[test]
    fn exclusive_requests_route_to_exclusive_lane() {
        let requests = [
            DaemonRequest::DebugTick {
                task: DebugTickTask::PeerSync,
                repeat: 1,
            },
            DaemonRequest::MeshJoin {
                token: "join-token".into(),
            },
            DaemonRequest::MeshInit {
                network: "alpha".into(),
            },
            DaemonRequest::MeshUp {
                network: "alpha".into(),
                allow_disconnected_bootstrap: false,
            },
            DaemonRequest::MeshDown,
            DaemonRequest::MeshDestroy {
                network: "alpha".into(),
            },
        ];

        for request in requests {
            assert_eq!(DaemonState::request_lane(&request), RequestLane::Exclusive);
        }
    }
}
