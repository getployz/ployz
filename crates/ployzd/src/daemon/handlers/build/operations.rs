use ployz_api::{BuildOperationListPayload, BuildOperationPayload, DaemonPayload};
pub(super) use ployz_build::operations::BuildOperationStore;
use ployz_model::OperationStatus;

use crate::daemon::DaemonState;

impl DaemonState {
    pub(super) fn build_operation_store(&self) -> BuildOperationStore {
        BuildOperationStore::new(self.data_dir.clone())
    }

    pub(crate) async fn handle_build_operation_list(&self) -> ployz_api::DaemonResponse {
        let operations = match self.build_operation_store().list() {
            Ok(records) => records,
            Err(err) => return self.err("BUILD_OPERATION_LIST_FAILED", err),
        };
        if operations.is_empty() {
            return self.ok_with_payload(
                "no build operations",
                Some(DaemonPayload::BuildOperationList(
                    BuildOperationListPayload { operations },
                )),
            );
        }

        let lines = operations
            .iter()
            .map(|record| {
                format!(
                    "{}  {}  {}  {}  {}",
                    record.id,
                    record.kind,
                    record.method,
                    record.status(),
                    record.stage
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        self.ok_with_payload(
            lines,
            Some(DaemonPayload::BuildOperationList(
                BuildOperationListPayload { operations },
            )),
        )
    }

    pub(crate) async fn handle_build_operation_get(&self, id: &str) -> ployz_api::DaemonResponse {
        let record = match self.build_operation_store().load(id) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err(
                    "BUILD_OPERATION_NOT_FOUND",
                    format!("build operation '{id}' not found"),
                );
            }
            Err(err) => return self.err("BUILD_OPERATION_GET_FAILED", err),
        };

        let payload = BuildOperationPayload {
            operation: record.clone(),
        };
        match serde_json::to_string_pretty(&record) {
            Ok(body) => self.ok_with_payload(body, Some(DaemonPayload::BuildOperation(payload))),
            Err(err) => self.err(
                "ENCODE_FAILED",
                format!("failed to encode build operation: {err}"),
            ),
        }
    }

    pub async fn recover_build_operations_on_startup(&self) {
        let store = self.build_operation_store();
        let records = match store.list() {
            Ok(records) => records,
            Err(err) => {
                tracing::warn!(error = %err, "build operation startup recovery: list failed");
                return;
            }
        };

        for mut record in records {
            if record.status() != OperationStatus::Running {
                continue;
            }
            if let Err(err) = store.update_status(
                &mut record,
                OperationStatus::Interrupted,
                Some(
                    "daemon restarted before build completed; retry the build or inspect any recorded artifact"
                        .into(),
                ),
            ) {
                tracing::warn!(error = %err, operation_id = %record.id, "build operation startup recovery: mark interrupted failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_model::{BuildLocation, BuildMethod, BuildOperationKind, MachineId, OperationStatus};
    use ployz_runtime_api::Identity;

    use crate::daemon::DaemonState;

    fn make_state() -> DaemonState {
        let data_dir = unique_temp_dir("ployz-build-op-state");
        let identity = Identity::generate(MachineId::new("founder"), [32; 32]);
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

    #[tokio::test]
    async fn startup_recovery_marks_running_operation_interrupted() {
        let state = make_state();
        let store = state.build_operation_store();
        let operation = store
            .begin(
                BuildOperationKind::Local,
                BuildMethod::Dockerfile,
                BuildLocation::Local,
                "building",
            )
            .expect("begin operation");

        state.recover_build_operations_on_startup().await;

        let recovered = state
            .build_operation_store()
            .load(&operation.id)
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(recovered.status(), OperationStatus::Interrupted);
        assert!(
            recovered
                .last_error()
                .expect("last error")
                .contains("daemon restarted")
        );
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
