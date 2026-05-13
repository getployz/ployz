use ployz_api::{DaemonPayload, ImageOperationListPayload, ImageOperationPayload};
pub(crate) use ployz_image::operations::ImageOperationStore;
use ployz_model::{ImageDigest, OperationStatus};

use crate::daemon::DaemonState;

impl DaemonState {
    pub(super) fn image_operation_store(&self) -> ImageOperationStore {
        ImageOperationStore::new(self.data_dir.clone())
    }

    pub(crate) async fn handle_image_operation_list(&self) -> ployz_api::DaemonResponse {
        let operations = match self.image_operation_store().list() {
            Ok(records) => records,
            Err(err) => return self.err("IMAGE_OPERATION_LIST_FAILED", err),
        };
        if operations.is_empty() {
            return self.ok_with_payload(
                "no image operations",
                Some(DaemonPayload::ImageOperationList(
                    ImageOperationListPayload { operations },
                )),
            );
        }

        let lines = operations
            .iter()
            .map(|record| {
                let digest = record
                    .digest
                    .as_ref()
                    .map(ImageDigest::as_str)
                    .unwrap_or("-");
                format!(
                    "{}  {}  {}  {}  {}",
                    record.id,
                    record.kind,
                    record.status(),
                    digest,
                    record.stage
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        self.ok_with_payload(
            lines,
            Some(DaemonPayload::ImageOperationList(
                ImageOperationListPayload { operations },
            )),
        )
    }

    pub(crate) async fn handle_image_operation_get(&self, id: &str) -> ployz_api::DaemonResponse {
        let record = match self.image_operation_store().load(id) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err(
                    "IMAGE_OPERATION_NOT_FOUND",
                    format!("image operation '{id}' not found"),
                );
            }
            Err(err) => return self.err("IMAGE_OPERATION_GET_FAILED", err),
        };

        let payload = ImageOperationPayload {
            operation: record.clone(),
        };
        match serde_json::to_string_pretty(&record) {
            Ok(body) => self.ok_with_payload(body, Some(DaemonPayload::ImageOperation(payload))),
            Err(err) => self.err(
                "ENCODE_FAILED",
                format!("failed to encode image operation: {err}"),
            ),
        }
    }

    pub async fn recover_image_operations_on_startup(&self) {
        let store = self.image_operation_store();
        let records = match store.list() {
            Ok(records) => records,
            Err(err) => {
                tracing::warn!(error = %err, "image operation startup recovery: list failed");
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
                    "daemon restarted before image operation completed; inspect image status before retrying"
                        .into(),
                ),
            ) {
                tracing::warn!(error = %err, operation_id = %record.id, "image operation startup recovery: mark interrupted failed");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_model::{ImageDigest, ImageOperationKind, MachineId, OperationStatus};
    use ployz_runtime_api::Identity;

    use crate::daemon::DaemonState;

    fn make_state() -> DaemonState {
        let data_dir = unique_temp_dir("ployz-image-op-state");
        let identity = Identity::generate(MachineId::new("founder"), [31; 32]);
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

    fn digest() -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid digest")
    }

    #[tokio::test]
    async fn startup_recovery_marks_running_operation_interrupted() {
        let state = make_state();
        let store = state.image_operation_store();
        let operation = store
            .begin(
                ImageOperationKind::Push,
                "streaming",
                Some(digest()),
                None,
                vec![MachineId::new("target-a")],
            )
            .expect("begin operation");

        state.recover_image_operations_on_startup().await;

        let recovered = state
            .image_operation_store()
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
