use ployz_api::{DaemonPayload, ImageOperationListPayload, ImageOperationPayload};
pub(crate) use ployz_image::operations::ImageOperationStore;
use ployz_model::ImageDigest;

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
        let recovery = match store.interrupt_running_after_daemon_restart() {
            Ok(recovery) => recovery,
            Err(err) => {
                tracing::warn!(error = %err, "image operation startup recovery: list failed");
                return;
            }
        };

        for failure in recovery.failures {
            tracing::warn!(
                error = %failure.error,
                operation_id = %failure.operation_id,
                "image operation startup recovery: mark interrupted failed"
            );
        }
    }
}
