mod control_dispatch;
mod debug;
mod deploy;
mod doctor;
mod invite;
mod lane;
pub(crate) mod machine;
mod mesh;
mod node_dispatch;
pub(crate) mod runtime;
mod status;
pub(crate) mod volume;

pub use lane::RequestLane;

#[cfg(test)]
mod tests {
    use super::RequestLane;
    use crate::daemon::DaemonState;
    use ployz_api::{
        BuildInputs, BuildLocalRequest, BuildMachineRequest, DaemonRequest, DebugTickTask,
        DeployApplyPreparedRequest, ImageDistributeRequest, ImagePushRequest,
        ImageReceiveSessionRequest, ImageReceivedImportRequest,
    };
    use ployz_model::{BuildMethod, DeployId, ImageDigest, MachineId};
    use ployz_node_api::NodeRequest;

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

    #[test]
    fn build_local_routes_to_shared_lane() {
        let lane = DaemonState::request_lane(&DaemonRequest::BuildLocal {
            request: BuildLocalRequest {
                method: BuildMethod::Dockerfile,
                context_dir: "/tmp/context".into(),
                image_name: "example/app:latest".into(),
                platform: None,
                push_target: None,
                distribute_targets: Vec::new(),
                inputs: BuildInputs::default(),
            },
        });

        assert_eq!(lane, RequestLane::Shared);
    }

    #[test]
    fn build_machine_routes_to_shared_lane() {
        let lane = DaemonState::request_lane(&DaemonRequest::BuildMachine {
            request: BuildMachineRequest {
                method: BuildMethod::Dockerfile,
                context_path: "/tmp/context".into(),
                image_name: "example/app:latest".into(),
                machine_id: MachineId::new("builder-a"),
                platform: None,
                inputs: BuildInputs::default(),
            },
        });

        assert_eq!(lane, RequestLane::Shared);
    }

    #[test]
    fn deploy_volume_clone_node_rpcs_route_to_shared_lane() {
        let clone_lane = DaemonState::node_request_lane(&NodeRequest::DeployNodeCloneVolume {
            namespace: "pr-39".into(),
            deploy_id: "deploy-1".into(),
            volume: "data".into(),
            source_namespace: "prod".into(),
            source_volume: "data".into(),
            snapshot: "snap".into(),
            quota: String::new(),
            mode: String::new(),
            owner: String::new(),
        });
        assert_eq!(clone_lane, RequestLane::Shared);

        let cleanup_lane =
            DaemonState::node_request_lane(&NodeRequest::DeployNodeCleanupUncommittedVolumeClone {
                namespace: "pr-39".into(),
                deploy_id: "deploy-1".into(),
                volume: "data".into(),
                source_namespace: "prod".into(),
                source_volume: "data".into(),
                snapshot: "snap".into(),
            });
        assert_eq!(cleanup_lane, RequestLane::Shared);
    }

    #[test]
    fn deploy_prepare_and_apply_prepared_route_to_shared_lane() {
        let prepare_lane = DaemonState::request_lane(&DaemonRequest::DeployPrepare {
            manifest_json: "{}".into(),
        });
        let apply_prepared_lane = DaemonState::request_lane(&DaemonRequest::DeployApplyPrepared {
            request: DeployApplyPreparedRequest {
                prepared_deploy_id: DeployId::new("prepare-1"),
            },
        });

        assert_eq!(prepare_lane, RequestLane::Shared);
        assert_eq!(apply_prepared_lane, RequestLane::Shared);
    }

    #[test]
    fn image_push_and_distribute_route_to_shared_lane() {
        let push_lane = DaemonState::request_lane(&DaemonRequest::ImagePush {
            request: ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId::new("machine-a")],
                platform: None,
                expected_digest: None,
            },
        });
        assert_eq!(push_lane, RequestLane::Shared);

        let distribute_lane = DaemonState::request_lane(&DaemonRequest::ImageDistribute {
            request: ImageDistributeRequest {
                digest: digest(),
                source_machine: MachineId::new("machine-a"),
                target_machines: vec![MachineId::new("machine-b")],
                platform: None,
            },
        });
        assert_eq!(distribute_lane, RequestLane::Shared);

        let receive_session_lane = DaemonState::request_lane(&DaemonRequest::ImageReceiveSession {
            request: ImageReceiveSessionRequest {
                operation_id: "image-push-1".into(),
                source_machine: MachineId::new("machine-a"),
                repository: Some("ployz/image-push-1".into()),
            },
        });
        assert_eq!(receive_session_lane, RequestLane::Shared);

        let received_import_lane = DaemonState::request_lane(&DaemonRequest::ImageReceivedImport {
            request: ImageReceivedImportRequest {
                operation_id: "image-distribute-1".into(),
                source_machine: MachineId::new("machine-a"),
                repository: "ployz/image-distribute-1".into(),
                reference: "image-distribute-1".into(),
                expected_digest: digest(),
                platform: None,
                repo_tags: vec!["example/app:latest".into()],
            },
        });
        assert_eq!(received_import_lane, RequestLane::Shared);
    }

    fn digest() -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid digest")
    }
}
