use std::time::Duration;

use async_trait::async_trait;
use ployz_api::{
    DaemonResponse, ImageDistributeRequest, ImagePushRequest, ImageReceiveSessionRequest,
    ImageReceivedImportRequest,
};
use ployz_image::push::{ImagePeerClient, ImageService, validate_image_distribute_request};
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
        self.image_service(active)
            .handle_image_push_with_backend(request, backend, self)
            .await
    }

    pub(crate) async fn handle_image_distribute(
        &self,
        request: &ImageDistributeRequest,
    ) -> DaemonResponse {
        if let Err(response) = validate_image_distribute_request(&self.identity.machine_id, request)
        {
            return response;
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
            return response;
        }
        let active = match self.require_active(
            "IMAGE_DISTRIBUTE_INACTIVE",
            "image distribute requires a running mesh",
        ) {
            Ok(active) => active,
            Err(response) => return *response,
        };
        self.image_service(active)
            .handle_image_distribute_with_backend(request, backend, self)
            .await
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
        self.image_service(active)
            .handle_image_receive_session(request)
            .await
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
        self.image_service(active)
            .handle_image_received_import_with_backend(request, backend)
            .await
    }
}

#[async_trait]
impl ImagePeerClient for DaemonState {
    async fn image_receive_session(
        &self,
        target_machine: &MachineId,
        request: ImageReceiveSessionRequest,
    ) -> Result<DaemonResponse, String> {
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
            .map_err(|error| {
                format!("request image receive session from {target_machine}: {error}")
            })
    }

    async fn image_distribute(
        &self,
        source_machine: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<DaemonResponse, String> {
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
            .map_err(|error| format!("request image distribute from {source_machine}: {error}"))
    }

    async fn image_received_import(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<DaemonResponse, String> {
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
            .map_err(|error| format!("request image import from {target_machine}: {error}"))
    }
}

#[cfg(test)]
#[path = "push_tests.rs"]
mod tests;
