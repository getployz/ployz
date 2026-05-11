mod local;
mod operations;

use ployz_api::BuildMachineRequest;

use crate::daemon::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_build_machine(
        &self,
        request: &BuildMachineRequest,
    ) -> ployz_api::DaemonResponse {
        if let Err(error) = local::plan_build_invocation(request.method, &request.inputs) {
            return self.err("BUILD_MACHINE_INPUT_INVALID", error);
        }
        self.err(
            "BUILD_MACHINE_UNSUPPORTED",
            "machine builds are not implemented in this release",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_api::{BuildEnvValue, BuildInputs};
    use ployz_runtime_api::Identity;
    use ployz_types::model::{BuildMethod, MachineId};
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }

    fn state() -> DaemonState {
        let data_dir = temp_dir("ployz-build-machine-test");
        let identity = Identity::generate(MachineId("founder".into()), [12; 32]);
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

    fn machine_request(inputs: BuildInputs) -> BuildMachineRequest {
        BuildMachineRequest {
            method: BuildMethod::Dockerfile,
            context_path: ".".into(),
            image_name: "example/app:latest".into(),
            machine_id: MachineId("builder-a".into()),
            platform: None,
            inputs,
        }
    }

    #[tokio::test]
    async fn build_machine_rejects_invalid_inputs_without_operation_record() {
        let state = state();
        let response = state
            .handle_build_machine(&machine_request(BuildInputs {
                env: BTreeMap::from([(
                    "BAD-NAME".into(),
                    BuildEnvValue::Plain { value: "x".into() },
                )]),
                docker_build_args: BTreeMap::new(),
            }))
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "BUILD_MACHINE_INPUT_INVALID");
        assert!(
            state
                .build_operation_store()
                .list()
                .expect("list operations")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn build_machine_returns_unsupported_without_operation_record() {
        let state = state();
        let response = state
            .handle_build_machine(&machine_request(BuildInputs::default()))
            .await;

        assert!(!response.ok);
        assert_eq!(response.code, "BUILD_MACHINE_UNSUPPORTED");
        assert!(
            state
                .build_operation_store()
                .list()
                .expect("list operations")
                .is_empty()
        );
    }
}
