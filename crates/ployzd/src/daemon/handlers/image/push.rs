use async_trait::async_trait;
use ployz_api::{
    DaemonPayload, DaemonResponse, ImageDistributeRequest, ImagePushRequest,
    ImageReceiveSessionRequest, ImageReceivedImportRequest,
};
use ployz_image::push::{ImagePeerClient, ImageService, validate_image_distribute_request};
use ployz_image::response::{ImageServicePayload, ImageServiceResponse};
use ployz_model::MachineId;
use ployz_node_runtime::{ImageNodePayload, ImageNodeResponse};
use ployz_runtime_api::RuntimeImageBackend;

use crate::daemon::{ActiveMesh, DaemonState};

impl DaemonState {
    fn image_service(&self, active: &ActiveMesh) -> ImageService {
        ImageService {
            local_machine: self.identity.machine_id.clone(),
            data_dir: self.data_dir.clone(),
            operation_store: self.image_operation_store(),
            registry: self.image_registry.clone(),
            store: active.mesh.store.clone(),
            receiver_bind_addr: active.image_receiver_bind_addr,
        }
    }

    pub(crate) async fn handle_image_push(&self, request: &ImagePushRequest) -> DaemonResponse {
        let backend = match self.runtime_image_backend().await {
            Ok(backend) => backend,
            Err(error) => return self.err("IMAGE_PUSH_RUNTIME_UNAVAILABLE", error),
        };
        self.handle_image_push_with_backend(request, backend.as_ref())
            .await
    }

    pub(crate) async fn handle_image_push_with_backend(
        &self,
        request: &ImagePushRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> DaemonResponse {
        if request.target_machines.is_empty() {
            return self.err(
                "IMAGE_PUSH_TARGET_REQUIRED",
                "image push requires at least one target machine",
            );
        }
        let active = match self
            .require_active("IMAGE_PUSH_INACTIVE", "image push requires a running mesh")
        {
            Ok(active) => active,
            Err(response) => return *response,
        };
        image_response_to_daemon_response(
            self.image_service(active)
                .handle_image_push_with_backend(request, backend, self)
                .await,
        )
    }

    pub(crate) async fn handle_image_distribute(
        &self,
        request: &ImageDistributeRequest,
    ) -> DaemonResponse {
        if let Err(response) = validate_image_distribute_request(&self.identity.machine_id, request)
        {
            return image_response_to_daemon_response(response);
        }
        let backend = match self.runtime_image_backend().await {
            Ok(backend) => backend,
            Err(error) => return self.err("IMAGE_DISTRIBUTE_RUNTIME_UNAVAILABLE", error),
        };
        self.handle_image_distribute_with_backend(request, backend.as_ref())
            .await
    }

    pub(crate) async fn handle_image_distribute_with_backend(
        &self,
        request: &ImageDistributeRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> DaemonResponse {
        if let Err(response) = validate_image_distribute_request(&self.identity.machine_id, request)
        {
            return image_response_to_daemon_response(response);
        }
        let active = match self.require_active(
            "IMAGE_DISTRIBUTE_INACTIVE",
            "image distribute requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        image_response_to_daemon_response(
            self.image_service(active)
                .handle_image_distribute_with_backend(request, backend, self)
                .await,
        )
    }

    pub(crate) async fn handle_image_receive_session(
        &self,
        request: &ImageReceiveSessionRequest,
    ) -> DaemonResponse {
        let active = match self.require_active(
            "IMAGE_RECEIVER_INACTIVE",
            "image receive session requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        image_response_to_daemon_response(
            self.image_service(active)
                .handle_image_receive_session(request)
                .await,
        )
    }

    pub(crate) async fn handle_image_received_import(
        &self,
        request: &ImageReceivedImportRequest,
    ) -> DaemonResponse {
        let backend = match self.runtime_image_backend().await {
            Ok(backend) => backend,
            Err(error) => return self.err("IMAGE_RECEIVED_IMPORT_RUNTIME_UNAVAILABLE", error),
        };
        self.handle_image_received_import_with_backend(request, backend.as_ref())
            .await
    }

    pub(crate) async fn handle_image_received_import_with_backend(
        &self,
        request: &ImageReceivedImportRequest,
        backend: &dyn RuntimeImageBackend,
    ) -> DaemonResponse {
        let active = match self.require_active(
            "IMAGE_RECEIVED_IMPORT_INACTIVE",
            "image received import requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        image_response_to_daemon_response(
            self.image_service(active)
                .handle_image_received_import_with_backend(request, backend)
                .await,
        )
    }
}

#[async_trait]
impl ImagePeerClient for DaemonState {
    async fn image_receive_session(
        &self,
        target_machine: &MachineId,
        request: ImageReceiveSessionRequest,
    ) -> Result<ImageServiceResponse, String> {
        self.request_image_receive_session(target_machine, request)
            .await
            .map(image_node_response_to_image_response)
    }

    async fn image_distribute(
        &self,
        source_machine: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<ImageServiceResponse, String> {
        self.request_image_distribute(source_machine, request)
            .await
            .map(image_node_response_to_image_response)
    }

    async fn image_received_import(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<ImageServiceResponse, String> {
        self.request_image_received_import(target_machine, request)
            .await
            .map(image_node_response_to_image_response)
    }
}

pub(super) fn image_response_to_daemon_response(response: ImageServiceResponse) -> DaemonResponse {
    match response {
        ImageServiceResponse::Success {
            message, payload, ..
        } => DaemonResponse::success(message, payload.map(image_payload_to_daemon_payload)),
        ImageServiceResponse::Error {
            code,
            message,
            payload,
        } => DaemonResponse::error(code, message, payload.map(image_payload_to_daemon_payload)),
    }
}

fn image_payload_to_daemon_payload(payload: ImageServicePayload) -> DaemonPayload {
    match payload {
        ImageServicePayload::ImageInspect(payload) => DaemonPayload::ImageInspect(payload),
        ImageServicePayload::ImagePush(payload) => DaemonPayload::ImagePush(payload),
        ImageServicePayload::ImageDistribute(payload) => DaemonPayload::ImageDistribute(payload),
        ImageServicePayload::ImageDistributeValidation(payload) => {
            DaemonPayload::ImageDistributeValidation(payload)
        }
        ImageServicePayload::ImageReceiveSession(payload) => {
            DaemonPayload::ImageReceiveSession(payload)
        }
        ImageServicePayload::ImageReceivedImport(payload) => {
            DaemonPayload::ImageReceivedImport(payload)
        }
    }
}

fn image_node_response_to_image_response(response: ImageNodeResponse) -> ImageServiceResponse {
    let success = response.is_ok();
    let code = response.code().to_string();
    let message = response.message().to_string();
    let payload = response
        .into_payload()
        .map(image_node_payload_to_image_payload);
    if success {
        return ImageServiceResponse::success(message, payload);
    }
    ImageServiceResponse::error(code, message, payload)
}

fn image_node_payload_to_image_payload(payload: ImageNodePayload) -> ImageServicePayload {
    match payload {
        ImageNodePayload::Distribute(payload) => {
            ImageServicePayload::ImageDistribute(payload.into())
        }
        ImageNodePayload::DistributeValidation(payload) => {
            ImageServicePayload::ImageDistributeValidation(payload.into())
        }
        ImageNodePayload::ReceiveSession(payload) => {
            ImageServicePayload::ImageReceiveSession(payload.into())
        }
        ImageNodePayload::ReceivedImport(payload) => {
            ImageServicePayload::ImageReceivedImport(payload.into())
        }
    }
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
