use std::time::Duration;

use async_trait::async_trait;
use ployz_api::{
    DaemonPayload, DaemonResponse, ImageDistributeRequest, ImagePushRequest,
    ImageReceiveSessionRequest, ImageReceivedImportRequest,
};
use ployz_image::push::{ImagePeerClient, ImageService, validate_image_distribute_request};
use ployz_image::response::{ImageServicePayload, ImageServiceResponse};
use ployz_model::MachineId;
use ployz_nats::{NodeCommandSubject, RpcPolicy};
use ployz_node_api::NodeRequest;
use ployz_runtime_api::RuntimeImageBackend;

use crate::daemon::{ActiveMesh, DaemonState};

const IMAGE_RECEIVED_IMPORT_RPC_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const IMAGE_DISTRIBUTE_RPC_TIMEOUT: Duration = Duration::from_secs(30 * 60);

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
        let client = self
            .nats_node_rpc_client()
            .await
            .map_err(|error| format!("connect node rpc for image receive session: {error}"))?;
        client
            .request(
                NodeCommandSubject::image_receive_session(target_machine),
                &NodeRequest::ImageReceiveSession { request },
            )
            .await
            .map(daemon_response_to_image_response)
            .map_err(|error| {
                format!("request image receive session from {target_machine}: {error}")
            })
    }

    async fn image_distribute(
        &self,
        source_machine: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<ImageServiceResponse, String> {
        let client = self
            .nats_node_rpc_client()
            .await
            .map_err(|error| format!("connect node rpc for image distribute: {error}"))?;
        client
            .with_policy(RpcPolicy {
                timeout: IMAGE_DISTRIBUTE_RPC_TIMEOUT,
            })
            .request(
                NodeCommandSubject::image_distribute(source_machine),
                &NodeRequest::ImageDistribute { request },
            )
            .await
            .map(daemon_response_to_image_response)
            .map_err(|error| format!("request image distribute from {source_machine}: {error}"))
    }

    async fn image_received_import(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<ImageServiceResponse, String> {
        let client = self
            .nats_node_rpc_client()
            .await
            .map_err(|error| format!("connect node rpc for image import: {error}"))?;
        client
            .with_policy(RpcPolicy {
                timeout: IMAGE_RECEIVED_IMPORT_RPC_TIMEOUT,
            })
            .request(
                NodeCommandSubject::image_received_import(target_machine),
                &NodeRequest::ImageReceivedImport { request },
            )
            .await
            .map(daemon_response_to_image_response)
            .map_err(|error| format!("request image import from {target_machine}: {error}"))
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

fn daemon_response_to_image_response(response: DaemonResponse) -> ImageServiceResponse {
    match response {
        DaemonResponse::Success {
            message, payload, ..
        } => ImageServiceResponse::success(
            message,
            payload.and_then(daemon_payload_to_image_payload),
        ),
        DaemonResponse::Error {
            code,
            message,
            payload,
        } => ImageServiceResponse::error(
            code,
            message,
            payload.and_then(daemon_payload_to_image_payload),
        ),
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

fn daemon_payload_to_image_payload(payload: DaemonPayload) -> Option<ImageServicePayload> {
    match payload {
        DaemonPayload::ImageInspect(payload) => Some(ImageServicePayload::ImageInspect(payload)),
        DaemonPayload::ImagePush(payload) => Some(ImageServicePayload::ImagePush(payload)),
        DaemonPayload::ImageDistribute(payload) => {
            Some(ImageServicePayload::ImageDistribute(payload))
        }
        DaemonPayload::ImageDistributeValidation(payload) => {
            Some(ImageServicePayload::ImageDistributeValidation(payload))
        }
        DaemonPayload::ImageReceiveSession(payload) => {
            Some(ImageServicePayload::ImageReceiveSession(payload))
        }
        DaemonPayload::ImageReceivedImport(payload) => {
            Some(ImageServicePayload::ImageReceivedImport(payload))
        }
        DaemonPayload::Doctor(_)
        | DaemonPayload::Status(_)
        | DaemonPayload::MachineList(_)
        | DaemonPayload::MachineRtt(_)
        | DaemonPayload::MachineAdd(_)
        | DaemonPayload::MachineStoragePromotion(_)
        | DaemonPayload::MachineUpdate(_)
        | DaemonPayload::MachineRemove(_)
        | DaemonPayload::MeshList(_)
        | DaemonPayload::MeshStatus(_)
        | DaemonPayload::MeshReady(_)
        | DaemonPayload::MeshSelfRecord(_)
        | DaemonPayload::MachineInviteList(_)
        | DaemonPayload::MachineOperationList(_)
        | DaemonPayload::MachineOperation(_)
        | DaemonPayload::AcmeHttp01Status(_)
        | DaemonPayload::DeployNamespaceSnapshot(_)
        | DaemonPayload::DeployPrepare(_)
        | DaemonPayload::BranchEnvironment(_)
        | DaemonPayload::BranchEnvironmentList(_)
        | DaemonPayload::DeployCandidateStarted(_)
        | DaemonPayload::DeployFailure(_)
        | DaemonPayload::ImageStatus(_)
        | DaemonPayload::ImageOperation(_)
        | DaemonPayload::ImageOperationList(_)
        | DaemonPayload::BuildResult(_)
        | DaemonPayload::BuildOperation(_)
        | DaemonPayload::BuildOperationList(_)
        | DaemonPayload::VolumeZfsInspect(_)
        | DaemonPayload::VolumeZfsSnapshot(_)
        | DaemonPayload::VolumeZfsClone(_)
        | DaemonPayload::VolumeZfsPeerSend(_)
        | DaemonPayload::VolumeZfsTransfer(_)
        | DaemonPayload::VolumeZfsTransferList(_)
        | DaemonPayload::RuntimeState(_) => None,
    }
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
