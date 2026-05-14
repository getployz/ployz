mod distribute;
mod operation_updates;
mod push_workflow;
mod receive;
mod transfer;
mod validation;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::response::{ImageServicePayload, ImageServiceResponse};
use async_trait::async_trait;
use ployz_model::{
    ImageDistributeRequest, ImageReceiveSessionRequest, ImageReceivedImportRequest, ImageRef,
    MachineId,
};
use ployz_store_api::StoreDriver;

use crate::operations::ImageOperationStore;
use crate::registry::ImageRegistry;
use transfer::default_receive_repository;
pub use validation::validate_image_distribute_request;

pub struct ImageService {
    pub local_machine: MachineId,
    pub data_dir: PathBuf,
    pub operation_store: ImageOperationStore,
    pub registry: ImageRegistry,
    pub store: StoreDriver,
    pub receiver_bind_addr: Option<SocketAddr>,
}

#[async_trait]
pub trait ImagePeerClient: Send + Sync {
    async fn image_receive_session(
        &self,
        target_machine: &MachineId,
        request: ImageReceiveSessionRequest,
    ) -> Result<ImageServiceResponse, String>;

    async fn image_distribute(
        &self,
        source_machine: &MachineId,
        request: ImageDistributeRequest,
    ) -> Result<ImageServiceResponse, String>;

    async fn image_received_import(
        &self,
        target_machine: &MachineId,
        request: ImageReceivedImportRequest,
    ) -> Result<ImageServiceResponse, String>;
}

impl ImageService {
    fn ok_with_payload(
        &self,
        message: impl Into<String>,
        payload: Option<ImageServicePayload>,
    ) -> ImageServiceResponse {
        ImageServiceResponse::success(message, payload)
    }

    fn err(&self, code: impl Into<String>, message: impl Into<String>) -> ImageServiceResponse {
        ImageServiceResponse::error(code, message, None)
    }

    fn err_with_payload(
        &self,
        code: impl Into<String>,
        message: impl Into<String>,
        payload: Option<ImageServicePayload>,
    ) -> ImageServiceResponse {
        ImageServiceResponse::error(code, message, payload)
    }
}

fn image_ref_from_tag(reference: &str, digest: ployz_model::ImageDigest) -> ImageRef {
    if reference.starts_with("sha256:") {
        return ImageRef::digest_only(digest);
    }

    match reference.rsplit_once(':') {
        Some((repository, tag)) if !repository.is_empty() && !tag.is_empty() => {
            ImageRef::repository_digest(repository, Some(tag.to_string()), digest)
        }
        _ => ImageRef::repository_digest(reference, None, digest),
    }
}

async fn cleanup_image_work_dir(path: &Path) {
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cleanup image work dir failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_model::ImageDigest;

    fn digest() -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid image digest")
    }

    #[test]
    fn image_ref_from_tag_preserves_repository_and_tag() {
        let digest = digest();

        let by_digest = image_ref_from_tag(digest.as_str(), digest.clone());
        assert_eq!(by_digest, ImageRef::digest_only(digest.clone()));

        let tagged = image_ref_from_tag("registry.example.com/app:stable", digest.clone());
        assert_eq!(
            tagged,
            ImageRef::repository_digest("registry.example.com/app", Some("stable".into()), digest)
        );
    }
}
