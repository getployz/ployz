use ployz_api::{BuildOperationListPayload, BuildOperationPayload, DaemonPayload};
pub(super) use ployz_build::operations::BuildOperationStore;

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
        let recovery = match store.interrupt_running_after_daemon_restart() {
            Ok(recovery) => recovery,
            Err(err) => {
                tracing::warn!(error = %err, "build operation startup recovery: list failed");
                return;
            }
        };

        for failure in recovery.failures {
            tracing::warn!(
                error = %failure.error,
                operation_id = %failure.operation_id,
                "build operation startup recovery: mark interrupted failed"
            );
        }
    }
}
