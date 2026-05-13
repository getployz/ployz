use ployz_api::{DaemonPayload, ImageStatusPayload, ImageStatusRequest};
use ployz_image::status::{image_status_records, render_image_record_line};

use crate::daemon::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_image_status(
        &self,
        request: &ImageStatusRequest,
    ) -> ployz_api::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_ACTIVE_MESH", "image status requires a running mesh");
        };
        let records = match image_status_records(&active.mesh.store, request).await {
            Ok(records) => records,
            Err(error) => return self.err("IMAGE_STATUS_FAILED", error),
        };

        if records.is_empty() {
            return self.ok_with_payload(
                "no image availability records",
                Some(DaemonPayload::ImageStatus(ImageStatusPayload { records })),
            );
        }

        let message = records
            .iter()
            .map(render_image_record_line)
            .collect::<Vec<_>>()
            .join("\n");
        self.ok_with_payload(
            message,
            Some(DaemonPayload::ImageStatus(ImageStatusPayload { records })),
        )
    }
}
