use ployz_api::ImageInspectRequest;
use ployz_image::inspect::{inspect_image_with_backend, inspect_target_machine};
use ployz_runtime_api::RuntimeImageBackend;
use std::sync::Arc;

use super::push::image_response_to_daemon_response;
use crate::daemon::DaemonState;

impl DaemonState {
    pub(crate) async fn handle_image_inspect(
        &self,
        request: &ImageInspectRequest,
    ) -> ployz_api::DaemonResponse {
        let Some(_) = self.active.as_ref() else {
            return self.err("NO_ACTIVE_MESH", "image inspect requires a running mesh");
        };
        if let Err(error) = inspect_target_machine(&self.identity.machine_id, request) {
            return self.err(error.code, error.message);
        }

        self.handle_image_inspect_with_backend(request, self.runtime_image_backend().await)
            .await
    }

    async fn handle_image_inspect_with_backend(
        &self,
        request: &ImageInspectRequest,
        backend_result: Result<Arc<dyn RuntimeImageBackend>, String>,
    ) -> ployz_api::DaemonResponse {
        let Some(active) = self.active.as_ref() else {
            return self.err("NO_ACTIVE_MESH", "image inspect requires a running mesh");
        };
        image_response_to_daemon_response(
            inspect_image_with_backend(
                &active.mesh.store,
                &self.image_operation_store(),
                &self.identity.machine_id,
                request,
                backend_result,
            )
            .await,
        )
    }
}
