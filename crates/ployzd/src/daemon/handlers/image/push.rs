use ployz_api::{ImageDistributeRequest, ImagePushRequest};

use crate::daemon::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_image_push(
        &self,
        request: &ImagePushRequest,
    ) -> ployz_api::DaemonResponse {
        let _ = request;
        self.err(
            "IMAGE_PUSH_UNIMPLEMENTED",
            "image push transport is not implemented yet",
        )
    }

    pub(crate) async fn handle_image_distribute(
        &self,
        request: &ImageDistributeRequest,
    ) -> ployz_api::DaemonResponse {
        let _ = request;
        self.err(
            "IMAGE_DISTRIBUTE_UNIMPLEMENTED",
            "image distribute transport is not implemented yet",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_runtime_api::Identity;
    use ployz_types::model::{ImageDigest, MachineId};

    #[tokio::test]
    async fn image_push_handler_returns_explicit_unimplemented_code() {
        let state = make_state();
        let response = state
            .handle_image_push(&ImagePushRequest {
                source_image: "example/app:latest".into(),
                target_machines: vec![MachineId("machine-a".into())],
                platform: None,
                expected_digest: None,
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_PUSH_UNIMPLEMENTED");
    }

    #[tokio::test]
    async fn image_distribute_handler_returns_explicit_unimplemented_code() {
        let state = make_state();
        let response = state
            .handle_image_distribute(&ImageDistributeRequest {
                digest: ImageDigest::try_new(format!("sha256:{}", "a".repeat(64)))
                    .expect("valid digest"),
                source_machine: MachineId("machine-a".into()),
                target_machines: vec![MachineId("machine-b".into())],
                platform: None,
            })
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "IMAGE_DISTRIBUTE_UNIMPLEMENTED");
    }

    fn make_state() -> DaemonState {
        let data_dir =
            std::env::temp_dir().join(format!("ployz-image-push-handler-{}", uuid::Uuid::new_v4()));
        let identity = Identity::generate(MachineId("founder".into()), [31; 32]);
        DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        )
    }
}
